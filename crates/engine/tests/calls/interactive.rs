use super::*;
use wes_engine::conversations::{ConversationIo, Input, InteractiveInvoker};
struct Conversation {
    timeline: Timeline,
    panic: bool,
}
impl InteractiveInvoker for Conversation {
    fn start(
        &self,
        _: Call,
        mut io: ConversationIo,
        cancellation: CancellationToken,
    ) -> InvocationFuture {
        self.timeline.lock().unwrap().push("conversation");
        assert!(!self.panic, "synthetic private provider panic");
        io.output.opened().unwrap();
        Box::pin(async move {
            tokio::select! {
                _ = cancellation.cancelled() => Err(InvocationError::Cancelled),
                input = io.receive() => {
                    assert!(matches!(input, Some(Input::Bytes(_))));
                    Ok(Value::new(Shape::Unknown, Data::Int(9), Provenance::default()).unwrap())
                }
            }
        })
    }
}
fn work(recording: &Recording, panic: bool) -> RunTicket<BoundCall> {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    providers.register_all_ports(
        ProviderDescription::new(
            "catalog",
            [Capability::new(["echo"], Shape::Unknown, Safety::Unsafe)],
            vec![],
        )
        .unwrap(),
        Arc::new(Echo {
            timeline: recording.timeline.clone(),
            result: None,
            panic: false,
        }),
        None,
        Some(Arc::new(Conversation {
            timeline: recording.timeline.clone(),
            panic,
        })),
    );
    let parsed = parse(&SourceText::new("test", "@interactive catalog echo"));
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
        panic!("invoke")
    };
    let bound = providers.bind_interactive(invocation).unwrap();
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
async fn interaction_records_calling_before_entry_and_called_after_join_without_recording_input() {
    let recording = Recording::new(sink());
    let ticket = recording.admit(work(&recording, false)).await;
    let run = ticket.run.clone();
    let active = CallExecutor::recorded(recording.journal.clone())
        .execute_interactive(ticket, CancellationToken::new())
        .await
        .ok()
        .unwrap();
    active.handle.send(b"synthetic-private-answer").unwrap();
    let report = active.completion.await;
    assert!(matches!(report.outcome, Outcome::Produced(_)));
    assert!(report.notices.is_empty());
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "conversation", "called"]
    );
    {
        let records = recording.records.lock().unwrap();
        assert_eq!(records.len(), 4);
        let Record::Recovery(RecoveryEntry::Calling(call)) = &records[2] else {
            panic!("calling")
        };
        assert_eq!(&call.run, run.id());
        assert!(!call.safe);
        assert!(!format!("{records:?}").contains("synthetic-private-answer"));
    }
    recording.finish().await;
}

#[tokio::test]
async fn interaction_rejects_absent_foreign_wrong_node_receipts_and_ephemeral_downgrade() {
    let recording = Recording::new(sink());
    let foreign = Recording::new(sink());
    let ticket = work(&recording, false);
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
            .execute_interactive(work, CancellationToken::new())
            .await
        else {
            panic!("denied")
        };
        failed(&report.outcome, "RUN005");
    }
    let admitted = recording.admit(ticket).await;
    let Err(report) = CallExecutor::ephemeral()
        .execute_interactive(admitted, CancellationToken::new())
        .await
    else {
        panic!("downgrade")
    };
    failed(&report.outcome, "RUN005");
    assert!(!recording.timeline.lock().unwrap().contains(&"conversation"));
    foreign.finish().await;
    recording.finish().await;
}

#[tokio::test]
async fn interaction_cancel_during_calling_joins_receipt_and_closes_attempt_without_entry() {
    let mut sink = sink();
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    sink.gate = Some(("calling", entered, gate));
    let recording = Recording::new(sink);
    let ticket = recording.admit(work(&recording, false)).await;
    let token = CancellationToken::new();
    let pending = tokio::spawn(
        CallExecutor::recorded(recording.journal.clone())
            .execute_interactive(ticket, token.clone()),
    );
    receive.await.unwrap();
    token.cancel();
    assert!(!pending.is_finished());
    release.send(()).unwrap();
    let Err(report) = pending.await.unwrap() else {
        panic!("cancelled")
    };
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "called"]
    );
    recording.finish().await;
}

#[tokio::test]
async fn interaction_panic_closes_recovery_and_completion_record_failure_preserves_result() {
    for panic in [false, true] {
        let mut sink = sink();
        if !panic {
            sink.fail = Some("called");
        }
        let recording = Recording::new(sink);
        let ticket = recording.admit(work(&recording, panic)).await;
        let active = CallExecutor::recorded(recording.journal.clone())
            .execute_interactive(ticket, CancellationToken::new())
            .await
            .ok()
            .unwrap();
        if !panic {
            active.handle.send(b"answer").unwrap();
        }
        let report = active.completion.await;
        if panic {
            failed(&report.outcome, "RUN001");
            assert!(report.notices.is_empty());
        } else {
            assert!(matches!(report.outcome, Outcome::Produced(_)));
            assert_eq!(report.notices.len(), 1);
            assert_eq!(report.notices[0].code(), "RUN005");
        }
        assert_eq!(recording.timeline.lock().unwrap().last(), Some(&"called"));
        recording.finish().await;
    }
}
