use super::*;
use wes_core::{Primitive, Provenance, Value};
use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
struct Events {
    count: usize,
    reject: bool,
}
impl StreamingInvoker for Events {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        if self.reject {
            sink.reject_item().unwrap();
        }
        for i in 0..self.count {
            sink.push(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(i as i64),
                    Provenance::default(),
                )
                .unwrap(),
            )
            .unwrap();
        }
        sink.opened().unwrap();
        Box::pin(async { Ok(()) })
    }
}
struct Receiver {
    seen: Arc<Mutex<Vec<(String, i64)>>>,
    gate: Option<Arc<Notify>>,
    entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}
impl Invoker for Receiver {
    fn invoke(&self, call: Call, cancel: CancellationToken) -> InvocationFuture {
        let Data::Int(n) = call.arguments["value"].data() else {
            panic!("individual event expected")
        };
        self.seen
            .lock()
            .unwrap()
            .push((call.capability.path.join(" "), *n));
        if let Some(entered) = &self.entered {
            let _ = entered.send(());
        }
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                tokio::select! {_ = gate.notified()=>(), _ = cancel.cancelled()=>()}
            }
            Ok(call.arguments["value"].clone())
        })
    }
}
struct NoFinite;
impl Invoker for NoFinite {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("stream must use streaming port")
    }
}
fn fixture(
    count: usize,
    reject: bool,
    gate: Option<Arc<Notify>>,
    entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> (Workspace, Arc<Mutex<Vec<(String, i64)>>>) {
    let mut base = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut cap = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    cap.streaming = true;
    base.register_provider_ports(
        ProviderDescription::new("events", [cap], vec![]).unwrap(),
        Arc::new(NoFinite),
        Some(Arc::new(Events { count, reject })),
    )
    .unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let caps = ["first", "second"].map(|name| {
        let mut c = Capability::new([name], Shape::Primitive(Primitive::Int), Safety::Unsafe);
        c.parameters = vec![Parameter::new(
            "value",
            Shape::Primitive(Primitive::Int),
            true,
        )];
        c
    });
    base.register_provider(
        ProviderDescription::new("effects", caps, vec![]).unwrap(),
        Arc::new(Receiver {
            seen: seen.clone(),
            gate,
            entered,
        }),
    )
    .unwrap();
    (base, seen)
}
async fn idle(handle: &SessionHandle) {
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn ordered_events_drain_after_fast_source_end_and_run_unsafe_suffix_once_per_event() {
    let (base, seen) = fixture(20, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = include_str!("../../../../examples/developer-workflow/ordered.wes").trim();
    let accepted = submit(&handle, "pipeline", source).await;
    assert_eq!(accepted.nodes.len(), 4, "{:?}", accepted.diagnostics);
    idle(&handle).await;
    let expected: Vec<_> = (0..20)
        .flat_map(|n| [("first".into(), n), ("second".into(), n + 1)])
        .collect();
    assert_eq!(*seen.lock().unwrap(), expected);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.graph.len(), 4);
    assert!(
        accepted
            .nodes
            .iter()
            .all(|n| snapshot.execution.graph.node(n).unwrap().state() == NodeState::Ready)
    );
    assert!(snapshot.execution.streaming.is_empty());
    stop(handle, task).await;
}
#[tokio::test]
async fn overflow_or_rejected_events_stop_with_explicit_error_before_suffix_entry() {
    for (count, reject, word) in [(600, false, "overflow"), (1, true, "rejected")] {
        let (base, seen) = fixture(count, reject, None, None);
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let accepted = submit(
            &handle,
            "pipeline",
            "events watch | effects first value:input",
        )
        .await;
        idle(&handle).await;
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .execution
                .graph
                .node(&accepted.nodes[0])
                .unwrap()
                .state(),
            NodeState::Failed
        );
        assert!(
            snapshot.execution.errors[&accepted.nodes[0]]
                .message()
                .contains(word)
        );
        assert!(seen.lock().unwrap().is_empty());
        stop(handle, task).await;
    }
}
#[tokio::test]
async fn stage_failure_stops_the_queue_and_cancel_work_joins_the_active_stage() {
    let (base, seen) = fixture(5, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle,"pipeline","events watch | effects first value:input | :calc { return div(1,0); } | effects second value:input").await;
    idle(&handle).await;
    assert_eq!(*seen.lock().unwrap(), vec![("first".into(), 0)]);
    stop(handle, task).await;
    let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let (base, seen) = fixture(5, false, Some(Arc::new(Notify::new())), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "pipeline",
        "events watch | effects first value:input | effects second value:input",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap();
    handle.cancel_work("pipeline".into()).await.unwrap();
    idle(&handle).await;
    assert_eq!(*seen.lock().unwrap(), vec![("first".into(), 0)]);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&accepted.nodes[1])
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn restored_stream_pipeline_is_held_until_explicit_source_restart() {
    use wes_engine::history::{
        CommandRecord, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint,
    };
    let (base, seen) = fixture(3, false, None, None);
    let source = "events watch | effects first value:input";
    let mut history = HistoryCapture::new(HistoryCaptureLimits::default());
    history
        .push(Record::Journal(JournalEntry::Command(CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "pipeline".into(),
            text: source.into(),
            replay: source.into(),
            nodes: ["id1", "id2"]
                .into_iter()
                .map(|n| wes_engine::graph::NodeId::new(n).unwrap())
                .collect(),
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
        })))
        .unwrap();
    let receipt = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: 1,
    };
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        history.finish(HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        }),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    idle(&handle).await;
    assert!(seen.lock().unwrap().is_empty());
    submit(&handle, "restart", ":refresh $id1").await;
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("first".into(), 0),
            ("first".into(), 1),
            ("first".into(), 2)
        ]
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn clean_empty_stream_skips_suffix_instead_of_waiting_forever_or_invoking_once() {
    let (base, seen) = fixture(0, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "pipeline",
        "events watch | effects first value:input",
    )
    .await;
    idle(&handle).await;
    assert!(seen.lock().unwrap().is_empty());
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&accepted.nodes[0])
            .unwrap()
            .state(),
        NodeState::Ready
    );
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&accepted.nodes[1])
            .unwrap()
            .state(),
        NodeState::Skipped
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn existing_stream_window_accepts_pure_pipeline_without_resubscription() {
    struct Live {
        starts: Arc<AtomicUsize>,
        ready: tokio::sync::mpsc::UnboundedSender<StreamSink>,
    }
    impl StreamingInvoker for Live {
        fn subscribe(&self, _: Call, sink: StreamSink, cancel: CancellationToken) -> StreamFuture {
            self.starts.fetch_add(1, Ordering::SeqCst);
            sink.push(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(1),
                    Provenance::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.push(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(2),
                    Provenance::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            self.ready.send(sink).unwrap();
            Box::pin(async move {
                cancel.cancelled().await;
                Ok(())
            })
        }
    }
    let starts = Arc::new(AtomicUsize::new(0));
    let (ready, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut base = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut cap = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    cap.streaming = true;
    base.register_provider_ports(
        ProviderDescription::new("events", [cap], vec![]).unwrap(),
        Arc::new(NoFinite),
        Some(Arc::new(Live {
            starts: starts.clone(),
            ready,
        })),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(&handle, "source", "events watch > events").await;
    let sink = receiver.recv().await.unwrap();
    idle(&handle).await;
    let reply = submit(
        &handle,
        "window",
        "$events | :calc pure { return input.filter(n => n > 1); } > selected",
    )
    .await;
    assert_eq!(reply.nodes.len(), 1, "{:?}", reply.diagnostics);
    idle(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["selected"].node].data(),
        &Data::List(vec![Data::Int(2)])
    );
    submit(&handle, "reactive", ":node policy $selected mode:reactive").await;
    let mut updates = handle.subscribe_updates().unwrap();
    sink.push(
        Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(3),
            Provenance::default(),
        )
        .unwrap(),
    )
    .unwrap();
    // Ingress is asynchronous: idle alone does not acknowledge an unpublished event.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = handle.snapshot().await.unwrap();
            if snapshot
                .execution
                .values
                .get(&snapshot.names["selected"].node)
                .is_some_and(|v| v.data() == &Data::List(vec![Data::Int(2), Data::Int(3)]))
            {
                break;
            }
            match updates.recv().await {
                Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => (),
                Err(error) => panic!("updates closed: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    submit(&handle, "cancel", ":cancel $events").await;
    stop(handle, task).await;
}

#[tokio::test]
async fn ordered_accumulation_executes_example_and_downstream_conversion_without_source_restart() {
    let (base, seen) = fixture(5, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "accumulation",
        include_str!("../../../../examples/ordered-accumulation/history.wes"),
    )
    .await;
    assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
    idle(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    let history = snapshot.names["history"].node.clone();
    let value = snapshot.execution.values[&history].clone();
    let Data::Record(record) = value.data() else {
        panic!()
    };
    assert_eq!(
        record["items"],
        Data::List(vec![Data::Int(4), Data::Int(6), Data::Int(8)])
    );
    let Data::Record(checkpoint) = &record["checkpoint"] else {
        panic!()
    };
    assert_eq!(checkpoint["sequence"], Data::Int(5));
    assert_eq!(checkpoint["dropped"], Data::Int(2));
    let converted = submit(
        &handle,
        "conversion",
        ":calc pure { return $history.items.filter(n => n > 4); } > selected",
    )
    .await;
    assert_eq!(converted.nodes.len(), 1, "{:?}", converted.diagnostics);
    idle(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["selected"].node].data(),
        &Data::List(vec![Data::Int(6), Data::Int(8)])
    );
    let refresh = submit(&handle, "refresh", ":refresh $history").await;
    assert!(
        refresh
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.values[&history], value);
    assert!(seen.lock().unwrap().is_empty());
    stop(handle, task).await;
}

#[tokio::test]
async fn ordered_accumulation_refuses_finite_inputs_and_invalid_options_before_source_entry() {
    for source in [
        ":calc { return [1]; } | :accumulate limit:3",
        "events watch | :accumulate limit:0",
        "events watch | :accumulate limit:3 overflow:ignore",
        ":accumulate limit:3",
    ] {
        let (base, seen) = fixture(5, false, None, None);
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let accepted = submit(&handle, "bad", source).await;
        assert!(
            accepted.nodes.is_empty(),
            "{source}: {:?}",
            accepted.diagnostics
        );
        assert!(
            accepted
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
        );
        assert!(seen.lock().unwrap().is_empty());
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn ordered_accumulation_default_overflow_stops_suffix() {
    let (base, seen) = fixture(5, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted=submit(&handle,"bounded","events watch | :accumulate limit:2 > history | :calc { return length(input.items); } | effects first value:input").await;
    assert_eq!(accepted.nodes.len(), 4, "{:?}", accepted.diagnostics);
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("first".into(), 1), ("first".into(), 2)]
    );
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.errors[&accepted.nodes[1]].code(),
        "ACC004"
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn ordered_accumulation_checkpoint_restores_held_without_delivery_or_resubscription() {
    use wes_engine::{history::CommandRecord, runtime::RestoredState, workspace::ReplayWorkspace};
    let source = "events watch | :accumulate limit:3 overflow:drop-oldest > history";
    let (base, _) = fixture(5, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted = submit(&handle, "history", source).await;
    idle(&handle).await;
    assert_eq!(accepted.nodes.len(), 2, "{:?}", accepted.diagnostics);
    let snapshot = handle.snapshot().await.unwrap();
    let node = snapshot.names["history"].node.clone();
    let value = snapshot.execution.values[&node].clone();
    let run = snapshot.execution.runs[&node].clone();
    stop(handle, task).await;
    let (base, _) = fixture(99, false, None, None);
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let command = CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "history".into(),
        text: source.into(),
        replay: source.into(),
        nodes: accepted.nodes.clone(),
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
    };
    let prepared = replay
        .prepare(&command, CancellationToken::new())
        .await
        .unwrap();
    replay.apply(prepared).unwrap();
    replay
        .hydrate(&node, RestoredState::Ready(value.clone()), Some(run))
        .unwrap();
    let mut restored = replay.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(restored.runtime().value_of(&node), Some(&value));
}

#[tokio::test]
async fn forks_run_actual_finite_example_and_select_failed_outcome_without_rerun() {
    let (base, seen) = fixture(0, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "forks",
        include_str!("../../../../examples/pipeline-forks/finite.wes"),
    )
    .await;
    assert!(
        !accepted
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        accepted.diagnostics
    );
    idle(&handle).await;
    let snap = handle.snapshot().await.unwrap();
    for (name, value) in [("doubled", 14), ("incremented", 8)] {
        assert_eq!(
            snap.execution.values[&snap.names[name].node].data(),
            &Data::Int(value)
        );
    }
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["broken"].node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
    for name in ["unreachable", "notCancelled"] {
        assert_eq!(
            snap.execution
                .graph
                .node(&snap.names[name].node)
                .unwrap()
                .state(),
            NodeState::Skipped
        );
    }
    assert!(
        snap.execution
            .values
            .contains_key(&snap.names["failureCode"].node)
    );
    assert!(seen.lock().unwrap().is_empty());
    stop(handle, task).await;
}

#[tokio::test]
async fn forks_isolate_failed_event_branch_and_preserve_order_and_filtered_checkpoint() {
    let (base, seen) = fixture(8, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "forks",
        include_str!("../../../../examples/pipeline-forks/events.wes"),
    )
    .await;
    assert!(
        !accepted
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        accepted.diagnostics
    );
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        (0..8)
            .flat_map(|n| [("first".into(), n), ("second".into(), n)])
            .collect::<Vec<_>>()
    );
    let snap = handle.snapshot().await.unwrap();
    let Data::Record(history) = snap.execution.values[&snap.names["evens"].node].data() else {
        panic!()
    };
    assert_eq!(
        history["items"],
        Data::List(vec![Data::Int(0), Data::Int(2), Data::Int(4), Data::Int(6)])
    );
    let Data::Record(checkpoint) = &history["checkpoint"] else {
        panic!()
    };
    assert_eq!(checkpoint["sequence"], Data::Int(4));
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["broken"].node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn forks_prepare_atomically_and_reject_effectful_predicates_and_implicit_joins() {
    for source in [
        "events watch | :fork { { effects first value:input } { unknown } }",
        ":calc { return 1; } | :fork { { :calc { return input; } } } | :calc { return input; }",
        ":def wrong(input: Int) -> Int as :calc pure { return input; }\nevents watch | :fork { when wrong { effects first value:input } }",
        ":def effect(input: Int) -> Bool as :calc { call('effects', ['first'], {value: input}); return true; }\nevents watch | :fork { when effect { effects first value:input } }",
    ] {
        let (base, seen) = fixture(7, false, None, None);
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        let reply = submit(&handle, "bad", source).await;
        assert!(reply.nodes.is_empty(), "{source}: {:?}", reply.diagnostics);
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
        );
        assert!(seen.lock().unwrap().is_empty());
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn direct_delivery_backpressures_large_burst_without_waiting_for_window_or_losing_events() {
    struct Burst;
    impl StreamingInvoker for Burst {
        fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
            Box::pin(async move {
                sink.opened().unwrap();
                for n in 0..1200 {
                    sink.send(
                        Value::new(
                            Shape::Primitive(Primitive::Int),
                            Data::Int(n),
                            Provenance::default(),
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                }
                Ok(())
            })
        }
    }
    let (mut base, seen) = fixture(0, false, None, None);
    let mut cap = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    cap.streaming = true;
    base.register_provider_ports(
        ProviderDescription::new("burst", [cap], vec![]).unwrap(),
        Arc::new(NoFinite),
        Some(Arc::new(Burst)),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "burst", "burst watch | effects first value:input").await;
    assert_eq!(reply.nodes.len(), 2, "{:?}", reply.diagnostics);
    // An open source is idle between events; completion is asserted via delivered effects.
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut updates = handle.subscribe_updates().unwrap();
        loop {
            if seen.lock().unwrap().len() == 1200 {
                break;
            }
            let _ = updates.recv().await;
        }
    })
    .await
    .unwrap();
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        (0..1200).map(|n| ("first".into(), n)).collect::<Vec<_>>()
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn native_limits_finish_only_their_branch_and_skip_counts_input_events() {
    let (base, seen) = fixture(20, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "limits", "events watch | :fork { { :stream limit:2 | effects first value:input } { :stream skip:2 | :stream limit:3 | effects second value:input } }").await;
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("first".into(), 0),
            ("first".into(), 1),
            ("second".into(), 2),
            ("second".into(), 3),
            ("second".into(), 4)
        ]
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn nested_branch_failure_does_not_poison_its_parent_sibling_or_next_event() {
    let (base, seen) = fixture(3, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "nested", "events watch | :fork { { :fork { { :calc { return 1 / 0; } } { effects first value:input } } } { effects second value:input } }").await;
    assert_eq!(reply.nodes.len(), 4, "{:?}", reply.diagnostics);
    idle(&handle).await;
    assert_eq!(
        *seen.lock().unwrap(),
        (0..3)
            .flat_map(|n| [("first".into(), n), ("second".into(), n)])
            .collect::<Vec<_>>()
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn cancelling_one_ordered_branch_joins_it_and_leaves_its_sibling_running() {
    let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(Notify::new());
    let (base, seen) = fixture(3, false, Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "cancel-branch", "events watch | :fork { { effects first value:input > blocked } { :stream accumulate limit:10 > healthy } }").await;
    assert_eq!(reply.nodes.len(), 3, "{:?}", reply.diagnostics);
    tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap();
    submit(&handle, "stop-one", ":cancel $blocked").await;
    idle(&handle).await;
    assert_eq!(*seen.lock().unwrap(), vec![("first".into(), 0)]);
    let snap = handle.snapshot().await.unwrap();
    let Data::Record(record) = snap.execution.values[&snap.names["healthy"].node].data() else {
        panic!()
    };
    assert_eq!(
        record["items"],
        Data::List(vec![Data::Int(0), Data::Int(1), Data::Int(2)])
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn native_field_operations_filter_and_project_without_reinterpreting_a_list_as_events() {
    let (base, _) = fixture(0, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "fields", ":calc { return {status:404, body: {ids:[1,2]}}; } | :fork { { :stream filter field:status equals:404 | :stream map field:body.ids > ids } { :stream filter field:status equals:500 | :calc { return 99; } > absent } }").await;
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
    idle(&handle).await;
    let snap = handle.snapshot().await.unwrap();
    assert_eq!(
        snap.execution.values[&snap.names["ids"].node].data(),
        &Data::List(vec![Data::Int(1), Data::Int(2)])
    );
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["absent"].node)
            .unwrap()
            .state(),
        NodeState::Skipped
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn finite_cancel_handler_runs_for_one_source_cancel_but_not_whole_work_cancel() {
    for whole_work in [false, true] {
        let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let (base, seen) = fixture(0, false, Some(Arc::new(Notify::new())), Some(entered));
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let accepted = submit(&handle, "finite-cancel", "effects first value:1 > source | :fork { on cancelled { :calc { return input.code; } > cancelledCode } on success { effects second value:input > successOnly } }").await;
        assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
        tokio::time::timeout(Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .unwrap();
        if whole_work {
            handle.cancel_work("finite-cancel".into()).await.unwrap();
        } else {
            submit(&handle, "cancel-source", ":cancel $source").await;
        }
        idle(&handle).await;
        let snapshot = handle.snapshot().await.unwrap();
        if whole_work {
            assert!(!snapshot.execution.values.contains_key(&accepted.nodes[1]));
        } else {
            assert_eq!(
                snapshot.execution.values[&accepted.nodes[1]].data(),
                &Data::Text("RUN003".into())
            );
        }
        assert_eq!(*seen.lock().unwrap(), vec![("first".into(), 1)]);
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn branch_removal_cannot_stop_another_actors_sibling_alias() {
    let (base, _) = fixture(3, false, None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    handle
        .observe_actor("agent".into(), "ui".into())
        .await
        .unwrap();
    let own = |cell: &str, text: &str| {
        SourceInput::new(cell.into(), text.into())
            .unwrap()
            .with_client("agent".into())
            .unwrap()
            .cooperative()
    };
    let accepted = handle.submit(own("branches", "events watch | :fork { { :calc { return input; } > firstBranch } { :calc { return input; } > secondBranch } }")).await.unwrap();
    assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
    idle(&handle).await;
    submit(&handle, "user-alias", "$secondBranch > protectedSibling").await;
    let reply = handle
        .submit(own(
            "remove-one",
            ":node remove $firstBranch scope:downstream",
        ))
        .await;
    match reply {
        Err(SessionError::Authority | SessionError::AccessDenied(_)) => (),
        Ok(reply) => assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.code == "AUT001"),
            "{:?}",
            reply.diagnostics
        ),
        other => panic!("expected refusal: {other:?}"),
    }
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.execution.graph.node(&accepted.nodes[1]).is_some());
    stop(handle, task).await;
}

#[tokio::test]
async fn ordered_accumulation_source_refresh_starts_new_history_after_end_cancel_or_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RestartEvents {
        subscriptions: Arc<AtomicUsize>,
        hold: bool,
    }
    impl StreamingInvoker for RestartEvents {
        fn subscribe(&self, _: Call, sink: StreamSink, cancel: CancellationToken) -> StreamFuture {
            let epoch = self.subscriptions.fetch_add(1, Ordering::SeqCst) + 1;
            let count = if epoch == 1 { 3 } else { 2 };
            for i in 0..count {
                sink.push(
                    Value::new(
                        Shape::Primitive(Primitive::Int),
                        Data::Int((epoch * 10 + i) as i64),
                        Provenance::default(),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            sink.opened().unwrap();
            let hold = self.hold;
            Box::pin(async move {
                if hold {
                    cancel.cancelled().await;
                }
                Ok(())
            })
        }
    }
    for mode in ["ended", "cancelled", "failed"] {
        let mut base = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let subscriptions = Arc::new(AtomicUsize::new(0));
        let mut cap = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
        cap.streaming = true;
        base.register_provider_ports(
            ProviderDescription::new("events", [cap], vec![]).unwrap(),
            Arc::new(NoFinite),
            Some(Arc::new(RestartEvents {
                subscriptions: subscriptions.clone(),
                hold: mode == "cancelled",
            })),
        )
        .unwrap();
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let limit = if mode == "failed" { 2 } else { 10 };
        let accepted = submit(&handle, "pipeline", &format!(
            "events watch | :accumulate limit:{limit} > history | :calc pure {{ return input.items; }} > rows"
        )).await;
        assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
        let source = &accepted.nodes[0];
        let history = &accepted.nodes[1];
        let rows = &accepted.nodes[2];
        // An open stream can be idle before its suffix finishes; wait for the actual result.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot().await.unwrap();
                assert!(
                    mode == "failed" || snapshot.execution.errors.is_empty(),
                    "{mode}: {:?}",
                    snapshot.execution.errors
                );
                let done = if mode == "failed" {
                    snapshot
                        .execution
                        .errors
                        .get(history)
                        .is_some_and(|error| error.code() == "ACC004")
                } else {
                    snapshot.execution.values.get(rows).is_some_and(|v| {
                        v.data() == &Data::List(vec![Data::Int(10), Data::Int(11), Data::Int(12)])
                    })
                };
                if done {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if mode == "cancelled" {
            handle.cancel_work("pipeline".into()).await.unwrap();
        }
        idle(&handle).await;
        let before = handle.snapshot().await.unwrap();
        let old_epoch = before.execution.values.get(history).and_then(|v| {
            let Data::Record(record) = v.data() else {
                return None;
            };
            let Data::Record(checkpoint) = &record["checkpoint"] else {
                return None;
            };
            Some(checkpoint["epoch"].clone())
        });
        let refreshed = submit(&handle, "restart", &format!(":refresh ${source}")).await;
        assert!(
            !refreshed
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{:?}",
            refreshed.diagnostics
        );
        let snapshot =
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let snapshot = handle.snapshot().await.unwrap();
                    if snapshot.execution.values.get(rows).is_some_and(|v| {
                        v.data() == &Data::List(vec![Data::Int(20), Data::Int(21)])
                    }) {
                        break snapshot;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        assert_eq!(subscriptions.load(Ordering::SeqCst), 2, "{mode}");
        let Data::Record(record) = snapshot.execution.values[history].data() else {
            panic!()
        };
        let Data::Record(checkpoint) = &record["checkpoint"] else {
            panic!()
        };
        assert_eq!(checkpoint["sequence"], Data::Int(2));
        assert_eq!(checkpoint["dropped"], Data::Int(0));
        assert_ne!(old_epoch.as_ref(), Some(&checkpoint["epoch"]));
        assert_eq!(snapshot.names["history"].node, *history);
        assert_eq!(snapshot.names["rows"].node, *rows);
        stop(handle, task).await;
    }
}
