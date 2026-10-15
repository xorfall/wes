use super::*;
use wes_engine::driver::publication::Reporter;

struct JoinedStarted {
    run: Run,
    token: CancellationToken,
    publish: oneshot::Sender<ExecutionReport>,
    accepted: oneshot::Receiver<bool>,
    finish: oneshot::Sender<Outcome>,
}
struct Joined {
    joined: mpsc::UnboundedSender<JoinedStarted>,
    ordinary: mpsc::UnboundedSender<Started>,
}
impl Executor<&'static str> for Joined {
    fn joined(&self, payload: &&'static str) -> bool {
        *payload == "joined"
    }
    fn execute(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        let (finish, receive) = oneshot::channel();
        self.ordinary
            .send(Started {
                ticket,
                token,
                finish,
            })
            .unwrap();
        Box::pin(async move { receive.await.unwrap().into() })
    }
    fn execute_joined(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
        _: progress::Reporter,
        reporter: Reporter,
    ) -> ExecutionFuture {
        let (publish, published) = oneshot::channel();
        let (accepted, accept) = oneshot::channel();
        let (finish, finished) = oneshot::channel();
        self.joined
            .send(JoinedStarted {
                run: ticket.run,
                token,
                publish,
                accepted: accept,
                finish,
            })
            .unwrap();
        Box::pin(async move {
            let mut report: ExecutionReport = published.await.unwrap();
            accepted.send(reporter.publish(&report).await).unwrap();
            report.outcome = finished.await.unwrap();
            report
        })
    }
}
fn joined_driver() -> (
    DriverHandle<&'static str>,
    DriverTask,
    ExecutionCapacity,
    mpsc::UnboundedReceiver<JoinedStarted>,
    mpsc::UnboundedReceiver<Started>,
) {
    let (joined, join) = mpsc::unbounded_channel();
    let (ordinary, body) = mpsc::unbounded_channel();
    let capacity = ExecutionCapacity::with_stream_limit(slots(1), slots(1)).unwrap();
    let (handle, task) = spawn_with_capacity(
        Runtime::new(),
        Arc::new(Joined { joined, ordinary }),
        capacity.clone(),
    )
    .unwrap();
    (handle, task, capacity, join, body)
}

#[tokio::test]
async fn one_acknowledged_result_releases_call_capacity_while_shutdown_retains_cleanup_owner() {
    let (handle, task, capacity, mut joined, mut ordinary) = joined_driver();
    let mut events = handle.subscribe().unwrap();
    let parent = add(&handle, "joined", vec![]).await;
    let child = add(&handle, "ordinary", vec![OutputRef::data(parent.clone())]).await;
    handle.command(Command::Start).await.unwrap();
    let owner = joined.recv().await.unwrap();
    assert_eq!(capacity.subscribe().borrow().operations.used, 1);
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    assert!(ordinary.try_recv().is_err());
    owner
        .publish
        .send(Outcome::Produced(value(7)).into())
        .unwrap();
    assert!(owner.accepted.await.unwrap());
    let next = ordinary.recv().await.unwrap();
    assert_eq!(next.ticket.run.node(), &child);
    assert_eq!(next.ticket.inputs[&parent], value(7));
    next.finish.send(Outcome::Produced(value(8))).unwrap();
    handle.wait_idle().await.unwrap();
    let observed = snapshot(&handle).await;
    assert_eq!(observed.values[&parent], value(7));
    assert!(observed.executing.contains(&parent));
    assert!(observed.streaming.is_empty());
    assert_eq!(capacity.subscribe().borrow().operations.used, 0);
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    terminal(&mut events, &parent, NodeState::Ready).await;
    handle.command(Command::Shutdown).await.unwrap();
    assert!(owner.token.is_cancelled());
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    owner
        .finish
        .send(Outcome::Failed(
            RuntimeCode::ExecutionFailed.error("cleanup", None),
        ))
        .unwrap();
    task.join().await.unwrap();
    assert_eq!(capacity.subscribe().borrow().streams.used, 0);
}

#[tokio::test]
async fn failed_and_unknown_results_are_visible_during_cleanup_and_late_success_cannot_replace_them()
 {
    for unknown in [true, false] {
        let (handle, task, capacity, mut joined, mut ordinary) = joined_driver();
        let parent = add(&handle, "joined", vec![]).await;
        let child = add(
            &handle,
            "ordinary",
            vec![OutputRef {
                node: parent.clone(),
                port: OutputPort::Error,
            }],
        )
        .await;
        handle.command(Command::Start).await.unwrap();
        let owner = joined.recv().await.unwrap();
        let error = if unknown {
            wes_core::ErrorValue::new(
                wes_core::ErrorId::new("synthetic").unwrap(),
                wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
                "Synthetic unknown",
                vec![],
                None,
            )
            .unwrap()
        } else {
            RuntimeCode::ExecutionFailed.error("Synthetic failure", None)
        };
        owner
            .publish
            .send(Outcome::Failed(error.clone()).into())
            .unwrap();
        assert!(owner.accepted.await.unwrap());
        let next = ordinary.recv().await.unwrap();
        assert_eq!(next.ticket.run.node(), &child);
        next.finish.send(Outcome::Produced(value(8))).unwrap();
        handle.wait_idle().await.unwrap();
        assert_eq!(snapshot(&handle).await.errors[&parent].code(), error.code());
        assert_eq!(capacity.subscribe().borrow().streams.used, 1);
        owner.finish.send(Outcome::Produced(value(99))).unwrap();
        let mut usage = capacity.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while usage.borrow_and_update().streams.used != 0 {
                usage.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let observed = snapshot(&handle).await;
        assert_eq!(observed.errors[&parent].code(), error.code());
        assert!(!observed.values.contains_key(&parent));
        assert!(joined.try_recv().is_err());
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn cancellation_after_publication_preserves_result_and_physical_capacity_until_join() {
    let (handle, task, capacity, mut joined, _) = joined_driver();
    let parent = add(&handle, "joined", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let owner = joined.recv().await.unwrap();
    let run = owner.run.id().clone();
    owner
        .publish
        .send(Outcome::Produced(value(7)).into())
        .unwrap();
    assert!(owner.accepted.await.unwrap());
    handle
        .command(Command::Cancel(parent.clone()))
        .await
        .unwrap();
    assert!(owner.token.is_cancelled());
    let before_join = snapshot(&handle).await;
    assert_eq!(before_join.values[&parent], value(7));
    assert_eq!(before_join.runs[&parent], run);
    assert!(!before_join.idle);
    assert_eq!(capacity.subscribe().borrow().streams.used, 1);
    owner
        .finish
        .send(Outcome::Cancelled(
            RuntimeCode::Cancelled.error("cleanup", None),
        ))
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.values[&parent], value(7));
    assert_eq!(capacity.subscribe().borrow().streams.used, 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn cleanup_panic_is_a_protected_notice_and_cannot_replace_published_business_result() {
    let (handle, task, capacity, mut joined, _) = joined_driver();
    let mut notices = handle.subscribe_notices().unwrap();
    let parent = add(&handle, "joined", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let owner = joined.recv().await.unwrap();
    let policy =
        wes_core::flow::FlowPolicy::default().confidential(wes_core::flow::Residence::Retainable);
    let result = value(7).with_provenance(Provenance::default().with_policy(&policy));
    owner
        .publish
        .send(Outcome::Produced(result.clone()).into())
        .unwrap();
    assert!(owner.accepted.await.unwrap());
    // Request cancellation first: the notice must survive retirement of the idle lease.
    handle
        .command(Command::Cancel(parent.clone()))
        .await
        .unwrap();
    assert!(owner.token.is_cancelled());
    drop(owner.finish); // Deterministically panics the fixture's awaited owner.
    let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.run, owner.run);
    assert_eq!(notice.error.policy(), &policy);
    assert_eq!(notice.error.code(), "ENV021");
    assert_eq!(
        notice.error.message(),
        "Private operation failed; details withheld."
    );
    handle.wait_idle().await.unwrap();
    let observed = snapshot(&handle).await;
    assert_eq!(observed.values[&parent], result);
    assert!(!observed.errors.contains_key(&parent));
    assert_eq!(capacity.subscribe().borrow().streams.used, 0);
    assert!(joined.try_recv().is_err());
    stop(handle, task).await;
}

#[tokio::test]
async fn cleanup_notices_are_announced_once_after_join_with_their_own_policy() {
    let (handle, task, _, mut joined, _) = joined_driver();
    let mut notices = handle.subscribe_notices().unwrap();
    let parent = add(&handle, "joined", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let owner = joined.recv().await.unwrap();
    let policy = wes_core::flow::FlowPolicy::default().private();
    let notice = RuntimeCode::ExecutionFailed
        .error("Synthetic cleanup notice", None)
        .with_policy(&policy);
    let mut report: ExecutionReport = Outcome::Produced(value(7)).into();
    report.notices.push(notice.clone());
    owner.publish.send(report).unwrap();
    assert!(owner.accepted.await.unwrap());
    assert!(notices.try_recv().is_err());
    owner.finish.send(Outcome::Skipped).unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.run, owner.run);
    assert_eq!(observed.error, notice);
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.values[&parent], value(7));
    assert!(notices.try_recv().is_err());
    stop(handle, task).await;
}
