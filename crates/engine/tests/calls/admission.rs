use super::*;
use wes_engine::{
    providers::LocalScope,
    streams::{StreamFuture, StreamSink, StreamingInvoker},
};

struct Source {
    entered: tokio::sync::mpsc::UnboundedSender<StreamSink>,
    finish: Mutex<Option<oneshot::Receiver<()>>>,
}
impl StreamingInvoker for Source {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        sink.opened().unwrap();
        self.entered.send(sink).unwrap();
        let finish = self.finish.lock().unwrap().take().unwrap();
        Box::pin(async move {
            finish.await.unwrap();
            Ok(())
        })
    }
}
async fn ready_source() -> (
    wes_engine::providers::OwnedSource,
    StreamSink,
    oneshot::Sender<()>,
) {
    let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let (finish, finished) = oneshot::channel();
    let mut cap = Capability::new(
        ["listen"],
        Shape::Primitive(wes_core::Primitive::Int),
        Safety::Safe,
    );
    cap.streaming = true;
    let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
    providers.register_ports(
        ProviderDescription::new("observer", [cap], vec![]).unwrap(),
        Arc::new(Echo {
            timeline: Arc::default(),
            result: None,
            panic: true,
        }),
        Some(Arc::new(Source {
            entered,
            finish: Mutex::new(Some(finished)),
        })),
    );
    let parsed = parse(&SourceText::new("synthetic", "observer listen"));
    let statement = &parsed.script.statements[0];
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let resolution = resolve(call, providers.catalogue()).unwrap();
    let Plan::NewNode(node) =
        plan::plan(&resolution, statement, &Guards::new(), &|_| None).unwrap()
    else {
        panic!("node")
    };
    let Task::Invoke(call) = node.task else {
        panic!("invocation")
    };
    let bound = providers.bind_stream(call).unwrap();
    let mut runtime = Runtime::new();
    runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let owner = CallExecutor::ephemeral()
        .prepare_source(ticket, CancellationToken::new())
        .await
        .unwrap()
        .start_owned()
        .unwrap();
    (owner, receive.recv().await.unwrap(), finish)
}

#[tokio::test]
async fn ready_source_loss_during_calling_receipt_blocks_entry_and_still_joins_recovery() {
    let mut sink = sink();
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    sink.gate = Some(("calling", entered, gate));
    let recording = Recording::new(sink);
    let ticket = recording.admit(ticket(&recording)).await;
    let prepared = CallExecutor::recorded(recording.journal.clone())
        .prepare_finite(ticket, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*recording.timeline.lock().unwrap(), ["command", "accepted"]);
    let (source, source_sink, finish) = ready_source().await;
    let prerequisite = source
        .wait_ready(Duration::from_secs(1), CancellationToken::new())
        .await
        .unwrap();
    let work = tokio::spawn(prepared.execute_after(prerequisite));
    receive.await.unwrap();
    source_sink.closing().unwrap();
    finish.send(()).unwrap();
    source.join().await.unwrap();
    assert!(
        !work.is_finished(),
        "accepted recovery append must be physically joined"
    );
    release.send(()).unwrap();
    let report = work.await.unwrap();
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "called"]
    );
    recording.finish().await;
}

#[tokio::test]
async fn preparation_rejects_foreign_admission_without_calling_or_external_effects() {
    let first = Recording::new(sink());
    let foreign = Recording::new(sink());
    let work = first.admit(ticket(&first)).await;
    assert!(
        CallExecutor::recorded(foreign.journal.clone())
            .prepare_finite(work, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(*first.timeline.lock().unwrap(), ["command", "accepted"]);
    assert!(foreign.timeline.lock().unwrap().is_empty());
    first.finish().await;
    foreign.finish().await;
}
