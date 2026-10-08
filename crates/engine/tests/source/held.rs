use super::*;
use wes_core::Primitive;
use wes_engine::{
    streams::{StreamFuture, StreamSink, StreamingInvoker},
    workspace::{Preparation, ReplayWorkspace},
};

struct Immediate(Arc<AtomicUsize>);
impl StreamingInvoker for Immediate {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        sink.push(
            wes_core::Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(7),
                Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
        sink.opened().unwrap();
        Box::pin(async { Ok(()) })
    }
}
fn fixture() -> (Workspace, Arc<AtomicUsize>) {
    let (mut workspace, calls) = workspace();
    let mut capability = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    capability.streaming = true;
    workspace
        .register_provider_ports(
            ProviderDescription::new("events", [capability], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
            Some(Arc::new(Immediate(calls.clone()))),
        )
        .unwrap();
    (workspace, calls)
}
#[tokio::test]
async fn held_source_declares_without_effects_and_refresh_preserves_the_first_event() {
    let (mut workspace, calls) = fixture();
    let prepared = plan(&workspace, "@hold events watch > logs", no_files()).await;
    assert_eq!(prepared.accepted().len(), 1, "{:?}", prepared.diagnostics());
    let applied = prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert!(applied.effects.is_empty());
    assert!(workspace.start(Duration::ZERO).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let parsed = wes_language::parse(&SourceText::new("refresh", ":refresh $logs"));
    let Preparation::Meta(meta) = workspace.prepare(&parsed.script.statements[0]).unwrap() else {
        panic!("refresh");
    };
    let control = workspace.prepare_control(meta).unwrap();
    let applied = workspace.apply_control(control, Duration::ZERO).unwrap();
    let ticket = applied
        .effects
        .into_iter()
        .find_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket)
            } else {
                None
            }
        })
        .expect("one explicit source admission");
    let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
    let (handle, task) = TaskExecutor::ephemeral()
        .execute_stream(ticket, CancellationToken::new())
        .await
        .unwrap();
    task.join().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle.snapshot().window.data(),
        &Data::List(vec![Data::Int(7)])
    );
}
#[tokio::test]
async fn held_source_replay_and_malformed_annotations_never_enter_a_provider() {
    let (workspace, calls) = fixture();
    let prepared = plan(&workspace, "@hold events watch > logs", no_files()).await;
    let record = prepared.record().unwrap().clone();
    let (base, restored_calls) = fixture();
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let prepared = replay
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    replay.apply(prepared).unwrap();
    assert!(replay.finish().start(Duration::ZERO).is_empty());
    assert_eq!(restored_calls.load(Ordering::SeqCst), 0);
    for text in [
        "@hold catalog echo value:x",
        "@hold :calc { return 1; }",
        "@hold @hold events watch",
        "@hold(foo) events watch",
        "@hold @interactive events watch",
    ] {
        let prepared = plan(&workspace, text, no_files()).await;
        assert!(prepared.accepted().is_empty(), "{text}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recording_launch_is_one_atomic_plan_with_a_setup_prerequisite_and_stable_replay_ids() {
    let (mut workspace, calls) = fixture();
    let text = "events watch > logs | :dataset record > recording";
    let prepared = plan(&workspace, text, no_files()).await;
    assert_eq!(prepared.accepted().len(), 1, "{:?}", prepared.diagnostics());
    let record = prepared.record().unwrap().clone();
    assert_eq!(record.nodes.len(), 2);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    let source = workspace.resolve("logs").unwrap().node;
    let setup = workspace.resolve("recording").unwrap().node;
    assert_eq!(source, record.nodes[0]);
    assert_eq!(setup, record.nodes[1]);
    let graph = workspace.runtime().graph();
    assert_eq!(
        graph
            .node(&source)
            .unwrap()
            .dependencies()
            .keys()
            .collect::<Vec<_>>(),
        [&setup]
    );
    assert!(graph.node(&setup).unwrap().dependencies().is_empty());
    let spawned = workspace
        .start(Duration::ZERO)
        .into_iter()
        .filter_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket.run.node().clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(spawned, [setup]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (base, replay_calls) = fixture();
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let prepared = replay
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(prepared.nodes().cloned().collect::<Vec<_>>(), record.nodes);
    replay.apply(prepared).unwrap();
    let mut restored = replay.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(restored.resolve("logs").unwrap().node, source);
    assert_eq!(replay_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_recording_launch_rejects_the_whole_plan_without_hidden_work() {
    let (workspace, calls) = fixture();
    for text in [
        "catalog echo value:x > leaked | :dataset record > recording",
        "events watch > leaked | :calc { return 1; } | :dataset record > recording",
        "@hold events watch > leaked | :dataset record > recording",
        "events watch > leaked | :dataset record from:next > recording",
        "events watch > leaked | :dataset record source:$leaked > recording",
    ] {
        let prepared = plan(&workspace, text, no_files()).await;
        assert!(
            prepared.nodes().next().is_none(),
            "{text}: {:?}",
            prepared.diagnostics()
        );
        assert!(prepared.accepted().is_empty(), "{text}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
