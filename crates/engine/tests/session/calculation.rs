use super::*;
#[tokio::test]
async fn calculation_calls_share_one_capacity_and_only_external_attempts_are_recorded() {
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
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle,"calc",":calc { let sum=0; for (const x of [1,2,3]) { sum=sum+call('catalog',['echo'],{value:x}); } return sum; } > total").await;
    assert!(
        reply.diagnostics.diagnostics.is_empty(),
        "{:?}",
        reply.diagnostics
    );
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let node = &snapshot.names["total"].node;
    assert_eq!(snapshot.execution.values[node].data(), &Data::Int(6));
    assert_eq!(snapshot.execution.graph.len(), 1);
    assert!(
        !snapshot
            .execution
            .graph
            .node(node)
            .unwrap()
            .payload()
            .traits()
            .repeatable
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let records = records.lock().unwrap().clone();
    let attempts: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Recovery(RecoveryEntry::Calling(c)) = r {
                Some(c)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(attempts.len(), 3);
    assert!(attempts.iter().all(|c| &c.node == node && c.cell == "calc"));
    assert_ne!(attempts[0].run, attempts[1].run);
    assert!(attempts[0].run.as_str().contains(":calc:"));
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(r, Record::Journal(JournalEntry::Command(_))))
            .count(),
        1
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
#[tokio::test]
async fn cancellation_joins_child_and_prevents_following_call_and_late_result() {
    let gate = Arc::new(Notify::new());
    let (entered, blocked) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply=submit(&handle,"calc",":calc { call('catalog',['echo'],{value:'blocked'}); return call('catalog',['echo'],{value:'late'}); } > result").await;
    assert!(reply.diagnostics.diagnostics.is_empty());
    blocked.await.unwrap();
    submit(&handle, "cancel", ":cancel $result").await;
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!snapshot.execution.values.contains_key(&reply.nodes[0]));
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&reply.nodes[0])
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    stop(handle, task).await;
}
#[tokio::test]
async fn committed_refresh_updates_pure_work_without_replaying_safe_provider_dependencies() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle, "setup", ":calc { return 1; } > seed\n:calc { return $seed + 1; } > pure\ncatalog echo value:$seed > external\n:calc { return call('catalog', ['echo'], {value:$seed}); } > nested").await;
    assert!(
        reply.diagnostics.diagnostics.is_empty(),
        "{:?}",
        reply.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let pure = before.names["pure"].node.clone();
    let old_run = before.execution.runs[&pure].clone();
    submit(&handle, "refresh", ":refresh $seed").await;
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    assert_eq!(after.execution.values[&pure].data(), &Data::Int(2));
    assert_ne!(after.execution.runs[&pure], old_run);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "SAFE external calls are not pure"
    );
    for name in ["external", "nested"] {
        assert_eq!(
            after
                .execution
                .graph
                .node(&after.names[name].node)
                .unwrap()
                .state(),
            NodeState::Stale
        );
    }
    submit(&handle, "manual", ":policy $pure mode:manual").await;
    submit(&handle, "refresh again", ":refresh $seed").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(
        handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(&pure)
            .unwrap()
            .state(),
        NodeState::Stale
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}
#[tokio::test]
async fn calculation_dependencies_and_pure_result_use_normal_workspace_outputs() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(
        &handle,
        "calc",
        ":calc { return 20; } > first\n:calc { return $first+22; } > answer",
    )
    .await;
    assert!(reply.diagnostics.diagnostics.is_empty());
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Int(42)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

struct FailSecond(Arc<AtomicUsize>);
impl Invoker for FailSecond {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if n == 1 {
                Err(wes_engine::providers::InvocationError::Failed(
                    wes_engine::runtime::RuntimeCode::ExecutionFailed
                        .error("synthetic second call failure", None),
                ))
            } else {
                Ok(call.arguments["value"].clone())
            }
        })
    }
}
#[tokio::test]
async fn successful_effect_survives_later_failure_and_duplicate_submission_does_not_retry() {
    let (mut base, calls) = workspace(None, None);
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Unsafe);
    capability
        .parameters
        .push(Parameter::new("value", Shape::Unknown, true));
    base.register_provider(
        ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
        Arc::new(FailSecond(calls.clone())),
    )
    .unwrap();
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
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let text = ":calc { for(const x of [1,2,3]) { call('catalog',['echo'],{value:x}); } return 9; } *> problem";
    let reply = submit(&handle, "effects", text).await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let snapshot = handle.snapshot().await.unwrap();
    let error = &snapshot.execution.errors[&reply.nodes[0]];
    assert_eq!(error.code(), "CAL008");
    assert!(error.message().contains("synthetic second call failure"));
    assert!(error.cause().is_some());
    submit(&handle, "effects", text).await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let produced: Vec<_> = records
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| {
            if let Record::Recovery(RecoveryEntry::Called { produced, .. }) = r {
                Some(*produced)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(produced, [true, false]);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
#[tokio::test]
async fn dynamic_provider_rules_refuse_entry_and_call_budget_refuses_attempt_1001() {
    use wes_core::capability::{DeclaredRule, Rule, RuleBasis};
    for limited in [false, true] {
        let (mut base, calls) = workspace(None, None);
        if !limited {
            let mut cap = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
            cap.parameters
                .push(Parameter::new("value", Shape::Unknown, true));
            cap.rules.push(DeclaredRule {
                rule: Rule::OneOf {
                    key: "value".into(),
                    values: ["allowed".into()].into(),
                },
                basis: RuleBasis::Documented { note: None },
            });
            base.register_provider(
                ProviderDescription::new("catalog", [cap], vec![]).unwrap(),
                Arc::new(Echo {
                    calls: calls.clone(),
                    gate: None,
                    entered: Mutex::new(None),
                }),
            )
            .unwrap();
        }
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let source = if limited {
            ":calc { for(const x of range(1001)) { call('catalog',['echo'],{value:x}); } return 0; }"
        } else {
            ":calc { let x='forbidden'; return call('catalog',['echo'],{value:x}); }"
        };
        let reply = submit(&handle, "limit", source).await;
        tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), if limited { 1000 } else { 0 });
        assert_eq!(
            handle.snapshot().await.unwrap().execution.errors[&reply.nodes[0]].code(),
            if limited { "CAL006" } else { "CAL004" }
        );
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn numeric_enum_arguments_use_their_exact_primitive_token() {
    use wes_core::{
        Primitive,
        capability::{DeclaredRule, Rule, RuleBasis},
    };
    let (mut base, calls) = workspace(None, None);
    let mut cap = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    cap.parameters.push(Parameter::new(
        "value",
        Shape::Primitive(Primitive::Int),
        true,
    ));
    cap.rules.push(DeclaredRule {
        rule: Rule::OneOf {
            key: "value".into(),
            values: ["1".into()].into(),
        },
        basis: RuleBasis::Documented { note: None },
    });
    base.register_provider(
        ProviderDescription::new("catalog", [cap], vec![]).unwrap(),
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
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(
        &handle,
        "enum",
        ":calc { let n=1; return call('catalog',['echo'],{value:n}); }",
    )
    .await;
    handle.wait_idle().await.unwrap();
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&reply.nodes[0]].data(),
        &Data::Int(1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}
#[tokio::test]
async fn iter_consumers_share_capacity_and_only_effectful_items_are_recorded() {
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
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle,"calc",":calc { let sum=0; for (const x of iter.items([1,2,3,4]).take(3)) { sum=sum+call('catalog',['echo'],{value:x}); } return sum; } > total").await;
    assert!(
        reply.diagnostics.diagnostics.is_empty(),
        "{:?}",
        reply.diagnostics
    );
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let node = &snapshot.names["total"].node;
    assert_eq!(snapshot.execution.values[node].data(), &Data::Int(6));
    assert_eq!(snapshot.execution.graph.len(), 1);
    assert!(
        !snapshot
            .execution
            .graph
            .node(node)
            .unwrap()
            .payload()
            .traits()
            .repeatable
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let records = records.lock().unwrap().clone();
    let attempts: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Recovery(RecoveryEntry::Calling(c)) = r {
                Some(c)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(attempts.len(), 3);
    assert!(attempts.iter().all(|c| &c.node == node && c.cell == "calc"));
    assert_ne!(attempts[0].run, attempts[1].run);
    assert!(attempts[0].run.as_str().contains(":calc:"));
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(r, Record::Journal(JournalEntry::Command(_))))
            .count(),
        1
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
#[tokio::test]
async fn iter_cancellation_joins_child_and_prevents_the_next_item_call() {
    let gate = Arc::new(Notify::new());
    let (entered, blocked) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply=submit(&handle,"calc",":calc { for(const x of iter.items(['blocked','late'])) {call('catalog',['echo'],{value:x});} return true; } > result").await;
    assert!(reply.diagnostics.diagnostics.is_empty());
    blocked.await.unwrap();
    submit(&handle, "cancel", ":cancel $result").await;
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!snapshot.execution.values.contains_key(&reply.nodes[0]));
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&reply.nodes[0])
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn later_iter_item_failure_preserves_prior_effects_and_collect_before_effects_refuses_all() {
    for (body, count, code) in [
        (
            "const xs=iter.checked(iter.items([1,'bad']),'Int'); for(const x of xs){call('catalog',['echo'],{value:x});} return true;",
            1,
            "CAL017",
        ),
        (
            "const xs=iter.checked(iter.items([1,'bad']),'Int').collect(); for(const x of xs){call('catalog',['echo'],{value:x});} return true;",
            0,
            "CAL017",
        ),
        (
            "const xs=iter.items([1]); return call('catalog',['echo'],{value:xs});",
            0,
            "CAL004",
        ),
    ] {
        let (base, calls) = workspace(None, None);
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Ephemeral,
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let reply = submit(&handle, "iter", &format!(":calc {{ {body} }} > result")).await;
        assert!(
            reply.diagnostics.diagnostics.is_empty(),
            "{:?}",
            reply.diagnostics
        );
        handle.wait_idle().await.unwrap();
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), count);
        assert!(snapshot.execution.errors.values().any(|e| e.code() == code));
        stop(handle, task).await;
    }
}

#[tokio::test]
async fn calculation_failure_context_reaches_session_errors_with_existing_trace_and_code() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(
        &handle,
        "context",
        ":calc { return [1].map(x => x.toString()); }",
    )
    .await;
    assert!(reply.diagnostics.diagnostics.is_empty());
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let error = &snapshot.execution.errors[&reply.nodes[0]];
    assert_eq!(error.code(), "CAL004");
    for fragment in ["'toString'", "Int", "text(value)"] {
        assert!(
            error.message().contains(fragment),
            "{} lacks {fragment}",
            error.message()
        );
    }
    assert!(error.locations().len() >= 2);
    assert_eq!(error.locations()[0].line, 1);
    assert!(error.issues().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!snapshot.execution.values.contains_key(&reply.nodes[0]));
    stop(handle, task).await;
}
