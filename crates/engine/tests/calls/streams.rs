use super::*;
use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
struct Stream(Timeline);
impl StreamingInvoker for Stream {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        self.0.lock().unwrap().push("subscribe");
        sink.opened().unwrap();
        Box::pin(async { Ok(()) })
    }
}
fn work(recording: &Recording) -> RunTicket<BoundCall> {
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.streaming = true;
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    providers.register_ports(
        ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
        Arc::new(Echo {
            timeline: recording.timeline.clone(),
            result: None,
            panic: false,
        }),
        Some(Arc::new(Stream(recording.timeline.clone()))),
    );
    let parsed = parse(&SourceText::new("test", "catalog echo"));
    let statement = &parsed.script.statements[0];
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let resolved = resolve(call, providers.catalogue()).unwrap();
    let Plan::NewNode(node) = plan::plan(&resolved, statement, &Guards::new(), &|_| None).unwrap()
    else {
        panic!("node")
    };
    let Task::Invoke(invocation) = node.task else {
        panic!("invocation")
    };
    let bound = providers.bind_stream(invocation).unwrap();
    let mut runtime = Runtime::new();
    runtime.add(bound.clone(), [], bound.traits()).unwrap();
    runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap()
}
#[tokio::test]
async fn streaming_requires_admission_but_does_not_create_finite_recovery_attempts() {
    let recording = Recording::new(sink());
    let ticket = recording.admit(work(&recording)).await;
    let (handle, task) = CallExecutor::recorded(recording.journal.clone())
        .execute_stream(ticket, CancellationToken::new())
        .await
        .unwrap_or_else(|_| panic!("admitted stream"));
    task.join().await.unwrap();
    assert_eq!(handle.snapshot().phase, wes_engine::streams::Phase::Ended);
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "subscribe"]
    );
    assert_eq!(recording.records.lock().unwrap().len(), 2);
    recording.finish().await;
}
#[tokio::test]
async fn streaming_rejects_missing_foreign_wrong_node_receipts_and_persistence_downgrade() {
    let recording = Recording::new(sink());
    let foreign = Recording::new(sink());
    let ticket = work(&recording);
    let wrong = recording
        .journal
        .admit(command(NodeId::new("other").unwrap()))
        .await
        .unwrap();
    let mut wrong_work = ticket.clone();
    wrong_work.payload = wrong_work.payload.with_admission(wrong);
    let foreign_work = foreign.admit(ticket.clone()).await;
    for work in [ticket.clone(), wrong_work, foreign_work] {
        let Err(report) = CallExecutor::recorded(recording.journal.clone())
            .execute_stream(work, CancellationToken::new())
            .await
        else {
            panic!("denied")
        };
        failed(&report.outcome, "RUN005");
    }
    let admitted = recording.admit(ticket).await;
    let Err(report) = CallExecutor::ephemeral()
        .execute_stream(admitted, CancellationToken::new())
        .await
    else {
        panic!("denied downgrade")
    };
    failed(&report.outcome, "RUN005");
    assert!(!recording.timeline.lock().unwrap().contains(&"subscribe"));
    foreign.finish().await;
    recording.finish().await;
}
