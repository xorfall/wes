use super::*;
use wes_core::capability::{Capability, Safety};
use wes_engine::{
    driver::publication::Reporter,
    providers::Call,
    streams::{self, StreamFuture, StreamSink, StreamingInvoker},
};

struct SourceEntered {
    run: Run,
    token: CancellationToken,
    sink: StreamSink,
    finish: oneshot::Sender<()>,
}
struct Source(mpsc::UnboundedSender<SourceEntered>);
impl StreamingInvoker for Source {
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        sink.push(value(99)).unwrap();
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.0
            .send(SourceEntered {
                run: call.run,
                token,
                sink,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(())
        })
    }
}
struct BusinessEntered {
    token: CancellationToken,
    publish: oneshot::Sender<ExecutionReport>,
    accepted: oneshot::Receiver<bool>,
    attached: bool,
}
struct Joined {
    source: Arc<Source>,
    entered: mpsc::UnboundedSender<BusinessEntered>,
}
impl Executor<&'static str> for Joined {
    fn joined(&self, payload: &&'static str) -> bool {
        *payload == "joined"
    }
    fn execute(&self, _: RunTicket<&'static str>, _: CancellationToken) -> ExecutionFuture {
        Box::pin(async { Outcome::Produced(value(42)).into() })
    }
    fn execute_joined(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
        _: progress::Reporter,
        reporter: Reporter,
    ) -> ExecutionFuture {
        let source = self.source.clone();
        let entered = self.entered.clone();
        Box::pin(async move {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            let (handle, task) = streams::spawn(
                Call {
                    run: ticket.run,
                    authority: Default::default(),
                    capability: Arc::new(capability),
                    arguments: Default::default(),
                },
                source,
                Default::default(),
                Default::default(),
                CancellationToken::new(),
            )
            .unwrap();
            let attached = reporter.attach_source(&handle).await;
            let (publish, published) = oneshot::channel();
            let (accept, accepted) = oneshot::channel();
            entered
                .send(BusinessEntered {
                    token,
                    publish,
                    accepted,
                    attached,
                })
                .ok()
                .unwrap();
            let report = published.await.unwrap();
            accept.send(reporter.publish(&report).await).unwrap();
            task.join().await.unwrap();
            report
        })
    }
}

#[tokio::test]
async fn acknowledged_auxiliary_attachment_never_publishes_a_window_and_keeps_cleanup_capacity_until_join()
 {
    let (source, mut sources) = mpsc::unbounded_channel();
    let (entered, mut business) = mpsc::unbounded_channel();
    let capacity = ExecutionCapacity::with_stream_limit(slots(1), slots(1)).unwrap();
    let (handle, task) = spawn_with_capacity(
        Runtime::new(),
        Arc::new(Joined {
            source: Arc::new(Source(source)),
            entered,
        }),
        capacity.clone(),
    )
    .unwrap();
    let parent = add(&handle, "joined", vec![]).await;
    let child = add(&handle, "ordinary", vec![OutputRef::data(parent.clone())]).await;
    handle.command(Command::Start).await.unwrap();
    let source = sources.recv().await.unwrap();
    let owner = business.recv().await.unwrap();
    assert!(owner.attached);
    assert_eq!(source.run.node(), &parent);
    let before = snapshot(&handle).await;
    assert!(!before.values.contains_key(&parent));
    assert!(!before.values.contains_key(&child));
    owner
        .publish
        .send(Outcome::Produced(value(7)).into())
        .unwrap();
    assert!(owner.accepted.await.unwrap());
    handle.wait_idle().await.unwrap();
    source.sink.push(value(100)).unwrap();
    let after = snapshot(&handle).await;
    assert_eq!(after.values[&parent], value(7));
    assert_eq!(after.values[&child], value(42));
    assert!(after.streaming.is_empty());
    assert!(after.executing.contains(&parent));
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    handle.command(Command::Shutdown).await.unwrap();
    assert!(owner.token.is_cancelled());
    assert!(
        !source.token.is_cancelled(),
        "parent retirement leaves the worker's cleanup source independently owned"
    );
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    source.finish.send(()).unwrap();
    joined.await.unwrap().unwrap();
    assert_eq!(capacity.subscribe().borrow().streams.used, 0);
}
