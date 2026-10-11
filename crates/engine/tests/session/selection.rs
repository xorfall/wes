use super::*;
use wes_engine::selection::Selection;

const TEMPLATE: &str = "catalog echo value:$rows.selected.value > detail";
async fn rows(handle: &SessionHandle) -> Selection {
    let result = submit(
        handle,
        "rows",
        ":calc pure { return [{value:'blocked'}, {value:'middle'}, {value:'last'}]; } > rows",
    )
    .await;
    assert!(
        result.diagnostics.diagnostics.is_empty(),
        "{:?}",
        result.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let node = snapshot.names["rows"].node.clone();
    Selection::Row {
        run: snapshot.execution.runs[&node].clone(),
        source: node,
        row: 0,
    }
}
fn activation(cell: &str, selector: &Selection, row: usize, repeat: bool) -> SourceInput {
    let Selection::Row { source, run, .. } = selector else {
        panic!()
    };
    let input = input(cell, TEMPLATE)
        .with_selection(Selection::Row {
            source: source.clone(),
            run: run.clone(),
            row,
        })
        .unwrap();
    if repeat {
        input.with_repeat("first".into(), true).unwrap()
    } else {
        input
    }
}

#[tokio::test]
async fn selection_captures_typed_input_without_changing_definition_and_refresh_repeats_it() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let request = activation("first", &selector, 2, false);
    let first = handle.submit(request.clone()).await.unwrap();
    assert_eq!(first.nodes.len(), 1, "{:?}", first.diagnostics);
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    let node = &first.nodes[0];
    assert_eq!(
        before.execution.values[node].data(),
        &Data::Text("last".into())
    );
    assert_eq!(
        before
            .execution
            .graph
            .node(node)
            .unwrap()
            .payload()
            .call()
            .unwrap()
            .selection_sources()
            .len(),
        1
    );
    assert!(Arc::ptr_eq(&first, &handle.submit(request).await.unwrap()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let second = handle
        .submit(activation("next", &selector, 1, true))
        .await
        .unwrap();
    assert_eq!(second.nodes, first.nodes);
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    assert_eq!(
        after
            .execution
            .graph
            .node(node)
            .unwrap()
            .payload()
            .call()
            .unwrap()
            .selection_sources()
            .len(),
        1
    );
    assert_eq!(
        after.execution.values[node].data(),
        &Data::Text("middle".into())
    );
    submit(&handle, "refresh", ":refresh $detail").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[node].data(),
        &Data::Text("middle".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn rapid_activations_replace_pending_inputs_and_wait_for_actual_exit() {
    rapid_activations(false).await;
}
#[tokio::test]
async fn reactive_policy_cannot_replay_superseded_invocation() {
    rapid_activations(true).await;
}
async fn rapid_activations(reactive: bool) {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(3).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let first = handle
        .submit(activation("first", &selector, 0, false))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    if reactive {
        submit(&handle, "reactive", ":policy $detail mode:reactive").await;
    }
    let h = handle.clone();
    let b = activation("second", &selector, 1, true);
    let pending_b = tokio::spawn(async move { h.submit(b).await });
    // A serialized observation ensures B has reached the actor before C is admitted.
    for _ in 0..100 {
        if handle
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "second")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let h = handle.clone();
    let c = activation("third", &selector, 2, true);
    let pending_c = tokio::spawn(async move { h.submit(c).await });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending_b)
            .await
            .unwrap()
            .unwrap(),
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gate.notify_one();
    let third = tokio::time::timeout(Duration::from_secs(5), pending_c)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(third.nodes, first.nodes);
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&first.nodes[0]].data(),
        &Data::Text("last".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn invalid_selection_does_not_cancel_running_work_and_cancel_clears_pending_intent() {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let first = handle
        .submit(activation("first", &selector, 0, false))
        .await
        .unwrap();
    receive.await.unwrap();
    assert!(
        handle
            .submit(activation("invalid", &selector, 99, true))
            .await
            .is_err()
    );
    assert_eq!(
        handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(&first.nodes[0])
            .unwrap()
            .state(),
        NodeState::Running
    );
    let h = handle.clone();
    let b = activation("pending", &selector, 2, true);
    let pending = tokio::spawn(async move { h.submit(b).await });
    for _ in 0..100 {
        if handle
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "pending")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    handle.cancel_work("first".into()).await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(SessionError::Cancelled)
    ));
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn declaration_edit_while_pending_does_not_rebind_frozen_request() {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    handle
        .submit(activation("first", &selector, 0, false))
        .await
        .unwrap();
    receive.await.unwrap();
    let h = handle.clone();
    let b = activation("pending", &selector, 2, true);
    let pending = tokio::spawn(async move { h.submit(b).await });
    for _ in 0..100 {
        if handle
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "pending")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    submit(&handle, "edit", ":change $detail value:edited").await;
    gate.notify_one();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(SessionError::RepeatRefused(_))
    ));
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn authoring_analysis_creates_no_work_and_does_not_activate_existing_selection() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let before = handle.snapshot().await.unwrap().execution.graph.len();
    assert_eq!(
        handle.selection_template(TEMPLATE.into()).await.unwrap(),
        Some(selector.source().clone())
    );
    handle.wait_idle().await.unwrap();
    assert_eq!(
        handle.snapshot().await.unwrap().execution.graph.len(),
        before
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn recorded_invocations_capture_arguments_before_calling_without_definition_edits() {
    let (base, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let first = handle
        .submit(activation("first", &selector, 1, false))
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    handle
        .submit(activation("second", &selector, 2, true))
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let saved = records.lock().unwrap().clone();
    let invocations: Vec<_> = saved
        .iter()
        .enumerate()
        .filter_map(|(at, record)| match record {
            Record::Journal(JournalEntry::Invocation(record)) => Some((at, record)),
            _ => None,
        })
        .collect();
    assert_eq!(invocations.len(), 2);
    for (at, invocation) in &invocations {
        assert_eq!(invocation.node, first.nodes[0]);
        assert_eq!(invocation.cell, "first");
        assert!(invocation.arguments.is_some());
        let calling = saved.iter().position(|entry| matches!(entry, Record::Recovery(RecoveryEntry::Calling(call)) if call.run == invocation.run)).unwrap();
        assert!(*at < calling);
    }
    assert_eq!(saved.iter().filter(|entry| matches!(entry, Record::Journal(JournalEntry::Command(command)) if command.cell == "first")).count(), 1);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn restored_selection_command_keeps_last_inputs_but_never_resumes_execution() {
    restore_last_inputs(false).await;
}
#[tokio::test]
async fn withheld_latest_inputs_never_fall_back_to_older_public_inputs() {
    restore_last_inputs(true).await;
}
async fn restore_last_inputs(withheld: bool) {
    let (base, _) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    handle
        .submit(activation("first", &selector, 2, false))
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let mut capture = wes_engine::history::HistoryCapture::new(Default::default());
    for record in records.lock().unwrap().clone() {
        capture.push(record).unwrap();
    }
    if withheld {
        let record = records
            .lock()
            .unwrap()
            .iter()
            .find_map(|entry| match entry {
                Record::Journal(JournalEntry::Invocation(record)) => Some(record.clone()),
                _ => None,
            })
            .unwrap();
        capture
            .push(Record::Journal(JournalEntry::Invocation(
                wes_engine::selection::InvocationRecord {
                    arguments: None,
                    run: wes_engine::runtime::RunId::new("later-withheld").unwrap(),
                    ..record
                },
            )))
            .unwrap();
    }
    let receipt = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: 100,
    };
    let image = capture.finish(wes_engine::history::HistoryCheckpoint {
        journal: receipt,
        recovery: receipt,
    });
    let (base, calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(2).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = submit(&handle, "refresh", ":refresh $detail").await;
    assert!(
        result.diagnostics.diagnostics.is_empty(),
        "{:?}",
        result.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    if withheld {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop(handle, task).await;
        return;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        snapshot.execution.values[&snapshot.names["detail"].node].data(),
        &Data::Text("last".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn unknown_remote_outcome_of_obsolete_run_blocks_pending_activation() {
    struct Uncertain {
        calls: Arc<AtomicUsize>,
        gate: Arc<Notify>,
        entered: Mutex<Option<oneshot::Sender<()>>>,
    }
    impl Invoker for Uncertain {
        fn invoke(&self, _call: Call, _token: CancellationToken) -> InvocationFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let gate = self.gate.clone();
            Box::pin(async move {
                gate.notified().await;
                Err(wes_engine::providers::InvocationError::Failed(
                    wes_core::ErrorValue::new(
                        wes_core::ErrorId::new("uncertain-fixture").unwrap(),
                        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
                        "Synthetic remote outcome is unknown",
                        vec![],
                        None,
                    )
                    .unwrap(),
                ))
            })
        }
    }
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (mut base, _) = workspace(None, None);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut capability = Capability::new(["read"], Shape::Unknown, Safety::Safe);
    capability
        .parameters
        .push(Parameter::new("value", Shape::Unknown, true));
    base.register_provider(
        ProviderDescription::new("uncertain", [capability], vec![]).unwrap(),
        Arc::new(Uncertain {
            calls: calls.clone(),
            gate: gate.clone(),
            entered: Mutex::new(Some(entered)),
        }),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    let template = "uncertain read value:$rows.selected.value > detail";
    handle
        .submit(
            input("first", template)
                .with_selection(selector.clone())
                .unwrap(),
        )
        .await
        .unwrap();
    receive.await.unwrap();
    let request = input("next", template)
        .with_selection(selector)
        .unwrap()
        .with_repeat("first".into(), true)
        .unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.submit(request).await });
    for _ in 0..100 {
        if handle
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|c| c.input.cell() == "next")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    gate.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    handle.wait_idle().await.unwrap();
    assert!(handle.remote_outcome_uncertain());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn ordinary_selected_fields_remain_data_and_selection_projection_is_typed() {
    let (mut base, calls) = workspace(None, None);
    let mut capability = Capability::new(["read"], Shape::Unknown, Safety::Safe);
    capability.parameters.push(Parameter::new(
        "value",
        Shape::Primitive(wes_core::Primitive::Int),
        true,
    ));
    base.register_provider(
        ProviderDescription::new("numbers", [capability], vec![]).unwrap(),
        Arc::new(Echo {
            calls: calls.clone(),
            gate: None,
            entered: Mutex::new(None),
        }),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    assert!(
        handle
            .selection_template("numbers read value:$rows.selected.value".into())
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    submit(
        &handle,
        "record",
        ":calc pure { return {selected:{value:7}}; } > record",
    )
    .await;
    handle.wait_idle().await.unwrap();
    assert_eq!(
        handle
            .selection_template("numbers read value:$record.selected.value".into())
            .await
            .unwrap(),
        None
    );
    submit(
        &handle,
        "normal",
        "numbers read value:$record.selected.value > normal",
    )
    .await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let result = handle
        .submit(
            input("full-row", "catalog echo value:$rows.selected > rowDetail")
                .with_selection(selector)
                .unwrap(),
        )
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(
        matches!(handle.snapshot().await.unwrap().execution.values[&result.nodes[0]].data(),Data::Record(fields) if fields.get("value")==Some(&Data::Text("blocked".into())))
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn superseded_queued_ticket_never_enters_the_provider() {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    submit(&handle, "blocker", "catalog echo value:blocked > blocker").await;
    receive.await.unwrap();
    let first = handle
        .submit(activation("first", &selector, 1, false))
        .await
        .unwrap();
    let h = handle.clone();
    let request = activation("replacement", &selector, 2, true);
    let pending = tokio::spawn(async move { h.submit(request).await });
    for _ in 0..100 {
        if handle
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "replacement")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&first.nodes[0]].data(),
        &Data::Text("last".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn refreshing_an_argument_source_cannot_replay_a_captured_invocation() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let selector = rows(&handle).await;
    submit(&handle, "other", ":calc pure { return 7; } > other").await;
    handle.wait_idle().await.unwrap();
    let first = handle
        .submit(
            input(
                "first",
                "catalog echo value:{chosen:$rows.selected.value,other:$other} > detail",
            )
            .with_selection(selector)
            .unwrap(),
        )
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap().execution.runs[&first.nodes[0]].clone();
    submit(&handle, "reactive", ":policy $detail mode:reactive").await;
    submit(&handle, "producer-refresh", ":refresh $other").await;
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    assert_eq!(after.execution.runs[&first.nodes[0]], before);
    assert_eq!(
        after.execution.graph.node(&first.nodes[0]).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    submit(&handle, "explicit-refresh", ":refresh $detail").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}
