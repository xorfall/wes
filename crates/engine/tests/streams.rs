use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationError},
    runtime::{Effect, ExecutionTraits, Outcome, Runtime},
    streams::{
        self, Limits, Phase, StreamError, StreamFuture, StreamHandle, StreamSink, StreamingInvoker,
    },
};

fn value(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
fn calls() -> (Call, Call) {
    let mut runtime = Runtime::new();
    let node = runtime
        .add(
            (),
            vec![],
            ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
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
    assert!(runtime.enter(&run));
    runtime.complete(&run, Outcome::Produced(value(0)), Duration::ZERO);
    let next = runtime
        .refresh(&node, Duration::ZERO)
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let mut capability = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    capability.streaming = true;
    let capability = Arc::new(capability);
    (
        Call {
            authority: Default::default(),
            run,
            capability: capability.clone(),
            arguments: Default::default(),
        },
        Call {
            authority: Default::default(),
            run: next,
            capability,
            arguments: Default::default(),
        },
    )
}
struct Started {
    sink: StreamSink,
    token: CancellationToken,
    finish: oneshot::Sender<Result<(), InvocationError>>,
}
struct Controlled {
    entered: mpsc::UnboundedSender<Started>,
    first: Option<Value>,
    acknowledge: bool,
}
impl StreamingInvoker for Controlled {
    fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        // Delivery may occur synchronously, before subscribe returns its future or opening is reported.
        if let Some(first) = &self.first {
            sink.push(first.clone()).unwrap();
        }
        if self.acknowledge {
            sink.opened().unwrap();
        }
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Started {
                sink,
                token,
                finish,
            })
            .unwrap();
        Box::pin(async move { receive.await.unwrap_or(Err(InvocationError::Cancelled)) })
    }
}
fn provider(
    first: Option<Value>,
    acknowledge: bool,
) -> (Arc<dyn StreamingInvoker>, mpsc::UnboundedReceiver<Started>) {
    let (entered, receive) = mpsc::unbounded_channel();
    (
        Arc::new(Controlled {
            entered,
            first,
            acknowledge,
        }),
        receive,
    )
}
async fn phase(handle: &StreamHandle, wanted: impl Fn(&Phase) -> bool) -> Arc<streams::Snapshot> {
    let mut updates = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if wanted(&snapshot.phase) {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn synchronous_first_item_survives_opening_and_final_items_flush_before_cadence() {
    let (call, _) = calls();
    let run = call.run.clone();
    let (provider, mut entered) = provider(Some(value(1)), true);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    let open = phase(&handle, |p| *p == Phase::Open).await;
    assert_eq!(open.run, run);
    assert_eq!(active.sink.opened(), Err(StreamError::AlreadyOpen));
    assert_eq!(open.window.data(), &Data::List(vec![Data::Int(1)]));
    active.sink.push(value(2)).unwrap();
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    let final_ = handle.snapshot();
    assert_eq!(final_.phase, Phase::Ended);
    assert_eq!(
        final_.window.data(),
        &Data::List(vec![Data::Int(1), Data::Int(2)])
    );
    assert_eq!(open.window.data(), &Data::List(vec![Data::Int(1)]));
    assert_eq!(active.sink.push(value(3)), Err(StreamError::Closed));
}

#[tokio::test]
async fn empty_window_has_declared_item_type_and_opening_must_be_acknowledged() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(None, false);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    assert_eq!(handle.snapshot().phase, Phase::Opening);
    assert_eq!(
        handle.snapshot().window.shape(),
        &Shape::List(Box::new(Shape::Primitive(Primitive::Int)))
    );
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    assert!(matches!(handle.snapshot().phase, Phase::Failed(_)));
}

#[tokio::test]
async fn cancellation_closes_admission_but_terminal_state_waits_for_physical_cleanup() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(None, true);
    let parent = CancellationToken::new();
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        parent.clone(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    phase(&handle, |p| *p == Phase::Open).await;
    let closing = tokio::spawn(task.shutdown());
    phase(&handle, |p| *p == Phase::Closing).await;
    assert!(active.token.is_cancelled());
    assert!(!parent.is_cancelled());
    assert!(!closing.is_finished());
    assert_eq!(active.sink.push(value(9)), Err(StreamError::Closed));
    active.finish.send(Ok(())).unwrap();
    closing.await.unwrap().unwrap();
    assert_eq!(handle.snapshot().phase, Phase::Cancelled);
}

#[tokio::test]
async fn old_callbacks_cannot_touch_a_replacement_run_or_keep_its_old_window_alive() {
    let (old_call, next_call) = calls();
    assert_eq!(old_call.run.node(), next_call.run.node());
    assert_ne!(old_call.run, next_call.run);
    let (provider, mut entered) = provider(None, true);
    let (old, old_task) = streams::spawn(
        old_call,
        provider.clone(),
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let previous = entered.recv().await.unwrap();
    old.cancel();
    let (next, next_task) = streams::spawn(
        next_call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let current = entered.recv().await.unwrap();
    assert_eq!(previous.sink.push(value(666)), Err(StreamError::Closed));
    current.sink.push(value(2)).unwrap();
    previous.finish.send(Ok(())).unwrap();
    old_task.join().await.unwrap();
    current.finish.send(Ok(())).unwrap();
    next_task.join().await.unwrap();
    assert_eq!(
        next.snapshot().window.data(),
        &Data::List(vec![Data::Int(2)])
    );
    assert_eq!(previous.sink.opened(), Err(StreamError::Closed));
}

#[tokio::test]
async fn many_arrivals_publish_one_bounded_window_and_report_omitted_items() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(None, true);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    phase(&handle, |p| *p == Phase::Open).await;
    let mut changes = handle.subscribe();
    changes.borrow_and_update();
    for n in 0..600 {
        active.sink.push(value(n)).unwrap();
    }
    // push does not broadcast one event per item; the cadence owns projection publication.
    assert!(!changes.has_changed().unwrap());
    tokio::time::timeout(Duration::from_secs(3), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let window = changes.borrow_and_update().clone();
    assert_eq!(window.omitted, 100);
    assert_eq!(
        window.window.data(),
        &Data::List((100..600).map(Data::Int).collect())
    );
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn an_oversized_item_cancels_even_when_provider_ignores_the_rejection() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(Some(value(7)), true);
    let limits = Limits {
        bytes: 4096.try_into().unwrap(),
        ..Default::default()
    };
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        limits,
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    phase(&handle, |p| *p == Phase::Open).await;
    let oversized = Value::new(
        Shape::Unknown,
        Data::Bytes(vec![0; 4096].into()),
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(active.sink.push(oversized), Err(StreamError::Capacity));
    assert!(active.token.is_cancelled());
    phase(&handle, |p| *p == Phase::Closing).await;
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    assert!(matches!(handle.snapshot().phase, Phase::Failed(_)));
    assert_eq!(
        handle.snapshot().window.data(),
        &Data::List(vec![Data::Int(7)])
    );
    assert_eq!(handle.snapshot().omitted, 0);
}

struct Panics;
impl StreamingInvoker for Panics {
    fn subscribe(&self, _: Call, _: StreamSink, _: CancellationToken) -> StreamFuture {
        panic!("private-provider-panic");
    }
}
#[tokio::test]
async fn synchronous_provider_panic_is_joined_without_exporting_its_payload() {
    let (call, _) = calls();
    let (handle, task) = streams::spawn(
        call,
        Arc::new(Panics),
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    task.join().await.unwrap();
    let snapshot = handle.snapshot();
    let Phase::Failed(error) = &snapshot.phase else {
        panic!("failure expected")
    };
    assert!(!error.message().contains("private-provider-panic"));
}

#[tokio::test]
async fn already_cancelled_parent_never_enters_provider_and_invalid_limits_never_spawn() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(None, true);
    let parent = CancellationToken::new();
    parent.cancel();
    let (handle, task) = streams::spawn(
        call.clone(),
        provider.clone(),
        Provenance::default(),
        Limits::default(),
        parent,
    )
    .unwrap();
    task.join().await.unwrap();
    assert_eq!(handle.snapshot().phase, Phase::Cancelled);
    assert!(entered.try_recv().is_err());
    let limits = Limits {
        items: 10_001.try_into().unwrap(),
        ..Default::default()
    };
    assert!(matches!(
        streams::spawn(
            call,
            provider,
            Provenance::default(),
            limits,
            CancellationToken::new()
        ),
        Err(StreamError::Invalid)
    ));
    assert!(entered.try_recv().is_err());
}

#[tokio::test]
async fn losing_the_task_owner_requests_cleanup_without_claiming_an_early_terminal_state() {
    let (call, _) = calls();
    let (provider, mut entered) = provider(None, true);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    phase(&handle, |p| *p == Phase::Open).await;
    drop(task);
    assert!(active.token.is_cancelled());
    phase(&handle, |p| *p == Phase::Closing).await;
    active.finish.send(Ok(())).unwrap();
    phase(&handle, |p| *p == Phase::Cancelled).await;
}

#[tokio::test]
async fn rejected_item_counts_are_run_bound_and_preserve_valid_window_and_attribution() {
    let (call, next) = calls();
    let (invoker, mut entered) = provider(Some(value(1)), true);
    let (handle, task) = streams::spawn(
        call,
        invoker.clone(),
        Provenance::default().with_fact("source", "fixture"),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    active.sink.reject_item().unwrap();
    active.sink.reject_item().unwrap();
    active.sink.push(value(2)).unwrap();
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.phase, Phase::Ended);
    assert_eq!(snapshot.rejected, 2);
    assert_eq!(snapshot.omitted, 0);
    assert_eq!(
        snapshot.window.data(),
        &Data::List(vec![Data::Int(1), Data::Int(2)])
    );
    assert_eq!(snapshot.window.provenance().fact("source"), Some("fixture"));
    assert_eq!(active.sink.reject_item(), Err(StreamError::Closed));
    let (replacement, task) = streams::spawn(
        next,
        invoker,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let active = entered.recv().await.unwrap();
    replacement.cancel();
    assert_eq!(active.sink.reject_item(), Err(StreamError::Closed));
    active.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    assert_eq!(replacement.snapshot().rejected, 0);
    assert_eq!(snapshot.rejected, 2);
}
