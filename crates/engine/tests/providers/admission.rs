use super::*;
use tokio::sync::{mpsc, oneshot};
use wes_core::ErrorValue;
use wes_engine::streams::{Phase, StreamFuture, StreamSink, StreamingInvoker};

struct SourceEntered {
    sink: StreamSink,
    token: CancellationToken,
    finish: oneshot::Sender<()>,
}
struct Source(mpsc::UnboundedSender<SourceEntered>);
impl StreamingInvoker for Source {
    fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        let (finish, receive) = oneshot::channel();
        self.0
            .send(SourceEntered {
                sink,
                token,
                finish,
            })
            .unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(())
        })
    }
}
fn source(providers: &mut Providers) -> (BoundCall, mpsc::UnboundedReceiver<SourceEntered>) {
    let (send, receive) = mpsc::unbounded_channel();
    let mut cap = Capability::new(["start"], Shape::Primitive(Primitive::Int), Safety::Safe);
    cap.streaming = true;
    providers.register_ports(
        ProviderDescription::new("observer", [cap], vec![]).unwrap(),
        Arc::new(Fake {
            result: None,
            calls: Arc::default(),
        }),
        Some(Arc::new(Source(send))),
    );
    (
        providers
            .bind_stream(invocation(providers, "observer start", &|_| None))
            .unwrap(),
        receive,
    )
}
async fn started(
    bound: BoundCall,
    receive: &mut mpsc::UnboundedReceiver<SourceEntered>,
) -> (OwnedSource, SourceEntered) {
    let prepared = CallExecutor::ephemeral()
        .prepare_source(ticket(bound, IndexMap::new()), CancellationToken::new())
        .await
        .unwrap();
    assert!(
        receive.try_recv().is_err(),
        "source preparation must have no external effect"
    );
    let owner = prepared.start_owned().unwrap();
    (owner, receive.recv().await.unwrap())
}
async fn ready(owner: &OwnedSource, entered: &SourceEntered) -> SourcePrerequisite {
    entered.sink.opened().unwrap();
    owner
        .wait_ready(Duration::from_secs(1), CancellationToken::new())
        .await
        .unwrap()
}
async fn prepared(providers: &Providers, token: CancellationToken) -> PreparedCall {
    let bound = providers
        .bind_finite(invocation(providers, "catalog echo value:hello", &|_| None))
        .unwrap();
    CallExecutor::ephemeral()
        .prepare_finite(ticket(bound, IndexMap::new()), token)
        .await
        .unwrap()
}

#[tokio::test]
async fn preparation_is_inert_and_one_ready_empty_source_admits_one_finite_call() {
    let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let finite = prepared(&providers, CancellationToken::new()).await;
    assert!(calls.lock().unwrap().is_empty());
    let (bound, mut receive) = source(&mut providers);
    let (owner, entered) = started(bound, &mut receive).await;
    let prerequisite = ready(&owner, &entered).await;
    assert!(matches!(owner.handle().snapshot().window.data(), Data::List(v) if v.is_empty()));
    let output = finite.execute_after(prerequisite).await;
    assert_eq!(produced(output.outcome).data(), &Data::Text("hello".into()));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        !entered.token.is_cancelled(),
        "ordinary completion must not silently own source stop"
    );
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

#[tokio::test]
async fn genuinely_consumed_private_scoped_arguments_keep_their_policy_with_a_ready_source() {
    let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let data = NodeId::new("captured-input").unwrap();
    let origin = wes_core::flow::ProviderReadOrigin::new(
        "00000000-0000-4000-8000-000000000086".into(),
        "a".repeat(64),
        "b".repeat(64),
    )
    .unwrap();
    let input_policy = wes_core::flow::FlowPolicy::default()
        .private()
        .with_provider_read(origin.clone());
    let input = int(7).with_provenance(Provenance::default().with_policy(&input_policy));
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{received:[$payload]}",
            &|name| (name == "payload").then(|| OutputRef::data(data.clone())),
        ))
        .unwrap();
    let finite = CallExecutor::ephemeral()
        .prepare_finite(
            ticket(bound, [(data, input)].into()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (bound, mut receive) = source(&mut providers);
    let (owner, entered) = started(bound, &mut receive).await;
    let prerequisite = ready(&owner, &entered).await;
    let result = produced(finite.execute_after(prerequisite).await.outcome);
    assert!(result.provenance().policy().is_private());
    assert!(
        result
            .provenance()
            .policy()
            .provider_reads()
            .contains(&origin)
    );
    let Data::Record(fields) = result.data() else {
        panic!("argument result")
    };
    assert_eq!(fields["received"], Data::List(vec![Data::Int(7)]));
    assert_eq!(calls.lock().unwrap().len(), 1);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

#[tokio::test]
async fn source_closing_cancellation_physical_end_and_lost_owner_refuse_preentry() {
    for phase in ["closing", "cancelled", "ended", "dropped"] {
        let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
        let calls = register(&mut providers, None);
        let finite = prepared(&providers, CancellationToken::new()).await;
        let (bound, mut receive) = source(&mut providers);
        let (owner, entered) = started(bound, &mut receive).await;
        let prerequisite = ready(&owner, &entered).await;
        match phase {
            "closing" => {
                entered.sink.closing().unwrap();
            }
            "cancelled" => owner.handle().cancel(),
            "ended" => {
                entered.finish.send(()).unwrap();
                owner.join().await.unwrap();
                assert!(matches!(
                    finite.execute_after(prerequisite).await.outcome,
                    Outcome::Failed(_)
                ));
                assert!(calls.lock().unwrap().is_empty());
                continue;
            }
            "dropped" => {
                let display = owner.handle().clone();
                drop(owner);
                assert!(matches!(
                    finite.execute_after(prerequisite).await.outcome,
                    Outcome::Failed(_)
                ));
                assert!(calls.lock().unwrap().is_empty());
                entered.finish.send(()).unwrap();
                let mut changes = display.subscribe();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while matches!(
                        changes.borrow_and_update().phase,
                        Phase::Opening | Phase::Open | Phase::Closing
                    ) {
                        changes.changed().await.unwrap();
                    }
                })
                .await
                .unwrap();
                continue;
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            finite.execute_after(prerequisite).await.outcome,
            Outcome::Failed(_)
        ));
        assert!(calls.lock().unwrap().is_empty(), "{phase}");
        entered.finish.send(()).unwrap();
        owner.join().await.unwrap();
    }
}

#[tokio::test]
async fn business_cancellation_between_preparation_and_readiness_admission_refuses_entry() {
    let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let cancellation = CancellationToken::new();
    let finite = prepared(&providers, cancellation.clone()).await;
    let (bound, mut receive) = source(&mut providers);
    let (owner, entered) = started(bound, &mut receive).await;
    let prerequisite = ready(&owner, &entered).await;
    cancellation.cancel();
    assert!(matches!(
        finite.execute_after(prerequisite).await.outcome,
        Outcome::Cancelled(_)
    ));
    assert!(calls.lock().unwrap().is_empty());
    assert!(!entered.token.is_cancelled());
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn readiness_deadline_is_only_a_wait_and_never_starts_or_cancels_work() {
    let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let finite = prepared(&providers, CancellationToken::new()).await;
    let (bound, mut receive) = source(&mut providers);
    let (owner, entered) = started(bound, &mut receive).await;
    assert!(
        owner
            .wait_ready(Duration::from_millis(5), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!entered.token.is_cancelled());
    let prerequisite = ready(&owner, &entered).await;
    assert!(matches!(
        finite.execute_after(prerequisite).await.outcome,
        Outcome::Produced(_)
    ));
    assert_eq!(calls.lock().unwrap().len(), 1);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

struct BusinessEntered {
    token: CancellationToken,
    finish: oneshot::Sender<Result<Value, InvocationError>>,
}
struct Business {
    entered: mpsc::UnboundedSender<BusinessEntered>,
    close: StreamSink,
}
impl Invoker for Business {
    fn invoke(&self, _: Call, token: CancellationToken) -> InvocationFuture {
        // Reenter the source synchronously. This would deadlock if an adapter
        // callback ran inside ReadyReceipt::admit while holding the source lock.
        self.close.closing().unwrap();
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(BusinessEntered { token, finish })
            .unwrap();
        Box::pin(async move { receive.await.unwrap() })
    }
}
#[tokio::test]
async fn postentry_source_loss_preserves_business_outcome_and_joins_once_without_replay() {
    for outcome in ["success", "error", "unknown"] {
        let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
        let (bound, mut source_receive) = source(&mut providers);
        let (owner, source_entered) = started(bound, &mut source_receive).await;
        let prerequisite = ready(&owner, &source_entered).await;
        let (send, mut receive) = mpsc::unbounded_channel();
        providers.register(
            metadata(),
            Arc::new(Business {
                entered: send,
                close: source_entered.sink.clone(),
            }),
        );
        let finite = prepared(&providers, CancellationToken::new()).await;
        let work = tokio::spawn(finite.execute_after(prerequisite));
        let business = receive.recv().await.unwrap();
        source_entered.finish.send(()).unwrap();
        owner.join().await.unwrap();
        assert!(!business.token.is_cancelled());
        assert!(
            !work.is_finished(),
            "source completion cannot detach business cleanup"
        );
        let result = match outcome {
            "success" => Ok(int(42)),
            _ => Err(InvocationError::Failed(
                ErrorValue::new(
                    wes_core::ErrorId::new("synthetic-error").unwrap(),
                    if outcome == "unknown" {
                        ErrorValue::REMOTE_OUTCOME_UNKNOWN
                    } else {
                        "SYN001"
                    },
                    "Synthetic failure",
                    vec![],
                    None,
                )
                .unwrap(),
            )),
        };
        business.finish.send(result).unwrap();
        let report = work.await.unwrap();
        match report.outcome {
            Outcome::Produced(value) => {
                assert_eq!(outcome, "success");
                assert_eq!(value.data(), &Data::Int(42));
            }
            Outcome::Failed(error) => {
                assert_ne!(outcome, "success");
                assert_eq!(
                    error.code(),
                    if outcome == "unknown" {
                        ErrorValue::REMOTE_OUTCOME_UNKNOWN
                    } else {
                        "SYN001"
                    }
                );
                assert_eq!(report.notices.len(), usize::from(outcome == "unknown"));
            }
            unexpected => panic!("{unexpected:?}"),
        }
        assert!(receive.try_recv().is_err(), "no business replay");
        assert!(source_receive.try_recv().is_err(), "no source restart");
    }
}
