use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use wes_core::{
    Data, Provenance, Shape, Value,
    capability::{Capability, Safety},
};
use wes_engine::{
    conversations::{
        self, Channel, ConversationError, ConversationIo, Input, InteractiveInvoker, Phase,
    },
    driver::CancellationToken,
    providers::{Call, InvocationError, InvocationFuture},
    runtime::{Effect, ExecutionTraits, Runtime},
};

fn call() -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: false,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability: Arc::new(Capability::new(["talk"], Shape::Unknown, Safety::Unsafe)),
        arguments: Default::default(),
    }
}
fn value() -> Value {
    Value::new(Shape::Unknown, Data::Int(7), Provenance::default()).unwrap()
}
struct Started {
    io: ConversationIo,
    token: CancellationToken,
    finish: oneshot::Sender<Result<Value, InvocationError>>,
}
struct Controlled {
    entered: mpsc::UnboundedSender<Started>,
    opened: bool,
}
impl InteractiveInvoker for Controlled {
    fn start(&self, _: Call, io: ConversationIo, token: CancellationToken) -> InvocationFuture {
        io.output.write(Channel::Stdout, b"prompt: ").unwrap();
        if self.opened {
            io.output.opened().unwrap();
        }
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Started { io, token, finish })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap_or(Err(InvocationError::Cancelled)) })
    }
}
fn provider(
    opened: bool,
) -> (
    Arc<dyn InteractiveInvoker>,
    mpsc::UnboundedReceiver<Started>,
) {
    let (entered, receive) = mpsc::unbounded_channel();
    (Arc::new(Controlled { entered, opened }), receive)
}
async fn phase(handle: &conversations::ConversationHandle, wanted: Phase) {
    let mut changes = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        while changes.borrow_and_update().phase != wanted {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn first_output_and_ephemeral_input_are_bounded_ordered_and_eof_is_final() {
    let (provider, mut started) = provider(true);
    let (handle, task) =
        conversations::spawn(call(), provider, 128, CancellationToken::new()).unwrap();
    let mut active = started.recv().await.unwrap();
    phase(&handle, Phase::Open).await;
    assert_eq!(handle.take_output().text, "prompt: ");
    assert_eq!(
        active.io.output.opened(),
        Err(ConversationError::AlreadyOpen)
    );
    assert_eq!(
        handle.send(&vec![0; conversations::input_bytes() + 1]),
        Err(ConversationError::Capacity)
    );
    for n in 0..conversations::INPUT_MESSAGES {
        handle.send(&[n as u8]).unwrap();
    }
    assert_eq!(handle.send(b"private input"), Err(ConversationError::Busy));
    assert_eq!(handle.eof(), Err(ConversationError::Busy));
    for n in 0..conversations::INPUT_MESSAGES {
        let Some(Input::Bytes(bytes)) = active.io.receive().await else {
            panic!("input bytes")
        };
        assert_eq!(bytes, [n as u8]);
    }
    handle.eof().unwrap();
    assert_eq!(handle.send(b"late"), Err(ConversationError::Closed));
    assert!(matches!(active.io.receive().await, Some(Input::Eof)));
    active
        .io
        .output
        .write(Channel::Stdout, b"last\xf0\x9f")
        .unwrap();
    active.finish.send(Ok(value())).unwrap();
    assert_eq!(task.join().await.unwrap().unwrap().data(), &Data::Int(7));
    assert_eq!(handle.snapshot().phase, Phase::Ended);
    assert_eq!(handle.take_output().text, "last�");
    assert_eq!(
        active.io.output.write(Channel::Stdout, b"late"),
        Err(ConversationError::Closed)
    );
}

#[tokio::test]
async fn cancel_revokes_callbacks_but_join_waits_for_physical_cleanup_and_other_runs_survive() {
    let (provider, mut started) = provider(true);
    let parent = CancellationToken::new();
    let (old, task) = conversations::spawn(call(), provider.clone(), 128, parent.clone()).unwrap();
    let old_started = started.recv().await.unwrap();
    let (next, next_task) = conversations::spawn(call(), provider, 128, parent.clone()).unwrap();
    let next_started = started.recv().await.unwrap();
    old.cancel();
    phase(&old, Phase::Closing).await;
    assert!(old_started.token.is_cancelled());
    assert!(!parent.is_cancelled());
    assert_eq!(old.send(b"private"), Err(ConversationError::Closed));
    assert_eq!(
        old_started.io.output.write(Channel::Stdout, b"stale"),
        Err(ConversationError::Closed)
    );
    next_started
        .io
        .output
        .write(Channel::Stdout, b"new")
        .unwrap();
    let mut joined = Box::pin(task.join());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut joined)
            .await
            .is_err()
    );
    old_started.finish.send(Ok(value())).unwrap();
    assert!(matches!(
        joined.await.unwrap(),
        Err(InvocationError::Cancelled)
    ));
    assert_eq!(next.take_output().text, "prompt: new");
    next_started.finish.send(Ok(value())).unwrap();
    next_task.join().await.unwrap().unwrap();
}

#[tokio::test]
async fn dropping_task_requests_cancel_and_does_not_fabricate_completion() {
    let (provider, mut started) = provider(true);
    let (handle, task) =
        conversations::spawn(call(), provider, 128, CancellationToken::new()).unwrap();
    let active = started.recv().await.unwrap();
    drop(task);
    phase(&handle, Phase::Closing).await;
    assert!(active.token.is_cancelled());
    active.finish.send(Ok(value())).unwrap();
    phase(&handle, Phase::Ended).await;
}

struct Panics;
impl InteractiveInvoker for Panics {
    fn start(&self, _: Call, _: ConversationIo, _: CancellationToken) -> InvocationFuture {
        panic!("synthetic private panic marker")
    }
}
#[tokio::test]
async fn missing_open_and_provider_panic_fail_without_exporting_panic_text() {
    let (provider, mut started) = provider(false);
    let (_, task) = conversations::spawn(call(), provider, 128, CancellationToken::new()).unwrap();
    started
        .recv()
        .await
        .unwrap()
        .finish
        .send(Ok(value()))
        .unwrap();
    let Err(InvocationError::Failed(error)) = task.join().await.unwrap() else {
        panic!("missing open")
    };
    assert!(error.message().contains("acknowledging"));
    let (_, task) =
        conversations::spawn(call(), Arc::new(Panics), 128, CancellationToken::new()).unwrap();
    let Err(InvocationError::Failed(error)) = task.join().await.unwrap() else {
        panic!("provider panic")
    };
    assert!(!error.message().contains("private"));
    assert!(error.message().contains("unexpectedly"));
}

#[tokio::test(start_paused = true)]
async fn output_notifies_at_bounded_cadence_and_human_wait_has_no_implicit_deadline() {
    let (provider, mut started) = provider(true);
    let (handle, task) =
        conversations::spawn(call(), provider, 8, CancellationToken::new()).unwrap();
    let active = started.recv().await.unwrap();
    phase(&handle, Phase::Open).await;
    handle.take_output();
    let before = handle.snapshot().output_revision;
    active
        .io
        .output
        .write(Channel::Stdout, &[b'x'; 1024])
        .unwrap();
    tokio::time::advance(Duration::from_millis(249)).await;
    tokio::task::yield_now().await;
    assert_eq!(handle.snapshot().output_revision, before);
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(handle.snapshot().output_revision > before);
    let output = handle.take_output();
    assert_eq!(output.text, "xxxxxxxx");
    assert_eq!(output.omitted_bytes, 1016);
    tokio::time::advance(Duration::from_secs(86_400)).await;
    tokio::task::yield_now().await;
    assert!(!active.token.is_cancelled());
    active.finish.send(Ok(value())).unwrap();
    task.join().await.unwrap().unwrap();
}
