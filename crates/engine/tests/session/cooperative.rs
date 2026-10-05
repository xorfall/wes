use super::*;

#[tokio::test]
async fn view_edits_require_scope_for_the_actual_instance_not_an_alias_receipt() {
    let (h, t) = start().await;
    submit(
        &h,
        "view-input",
        ":calc { return {view: \"metric\", value: 1000}; } > sample",
    )
    .await;
    h.wait_idle().await.unwrap();
    let created = submit(&h, "user-view", ":view create Metric input:$sample > chart").await;
    h.wait_idle().await.unwrap();
    let view = created.nodes[0].clone();
    let edit = h
        .submit(actor(
            "denied-edit",
            "a",
            ":view bind $chart input:$sample > receipt",
        ))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    assert!(
        h.snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .contains_key(&edit.nodes[0])
    );
    assert_eq!(
        h.view_frame(view.clone()).await.unwrap().instances[0].revision,
        0
    );
    h.grant_work("a".into(), vec!["user-view".into()])
        .await
        .unwrap();
    let allowed = h
        .submit(actor(
            "allowed-edit",
            "a",
            ":view bind $chart input:$sample > receipt2",
        ))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    assert!(
        h.snapshot()
            .await
            .unwrap()
            .execution
            .values
            .contains_key(&allowed.nodes[0])
    );
    assert_eq!(
        h.view_frame(view.clone()).await.unwrap().instances[0].revision,
        1
    );
    h.grant_work("a".into(), vec![]).await.unwrap();
    let alias = h
        .submit(actor(
            "alias-edit",
            "a",
            ":view bind $receipt2 input:$sample",
        ))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    assert!(
        h.snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .contains_key(&alias.nodes[0])
    );
    assert_eq!(h.view_frame(view).await.unwrap().instances[0].revision, 1);
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}
fn actor(cell: &str, who: &str, text: &str) -> SourceInput {
    SourceInput::new(cell.into(), text.into())
        .unwrap()
        .with_client(who.into())
        .unwrap()
        .cooperative()
}
fn denied(reply: session::SubmissionReply) {
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
        other => panic!("expected authority refusal: {other:?}"),
    }
}
async fn start() -> (SessionHandle, session::SessionTask) {
    let (base, _) = workspace(None, None);
    let (h, t) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    h.observe_actor("a".into(), "ui".into()).await.unwrap();
    h.observe_actor("b".into(), "ui".into()).await.unwrap();
    (h, t)
}
#[tokio::test]
async fn source_disclosure_is_exact_revocable_and_independent_of_work_authority() {
    let (h, t) = start().await;
    let user_text = ":calc { return 41; } > user_source";
    submit(&h, "user-source", user_text).await;
    let own_text = ":inspect $does_not_exist";
    h.submit(actor("own-failed", "a", own_text)).await.unwrap();
    h.wait_idle().await.unwrap();
    assert_eq!(
        h.read_source("a".into(), "own-failed".into())
            .await
            .unwrap()
            .as_deref(),
        Some(own_text)
    );
    assert_eq!(
        h.read_source("b".into(), "own-failed".into())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        h.read_source("a".into(), "user-source".into())
            .await
            .unwrap(),
        None
    );
    h.grant_work("a".into(), vec!["user-source".into()])
        .await
        .unwrap();
    assert_eq!(
        h.read_source("a".into(), "user-source".into())
            .await
            .unwrap(),
        None
    );
    h.grant_sources("a".into(), vec!["user-source".into()])
        .await
        .unwrap();
    h.grant_work("a".into(), vec![]).await.unwrap(); // does not revoke separate read scope
    assert_eq!(
        h.read_source("a".into(), "user-source".into())
            .await
            .unwrap()
            .as_deref(),
        Some(user_text)
    );
    assert_eq!(
        h.read_source("b".into(), "user-source".into())
            .await
            .unwrap(),
        None
    );
    denied(
        h.submit(actor(
            "read-not-write",
            "a",
            ":node remove $user_source scope:downstream",
        ))
        .await,
    );
    assert!(
        h.grant_sources("a".into(), vec!["missing-cell".into()])
            .await
            .is_err()
    );
    // A rejected replacement leaves the previous exact scope intact.
    assert!(
        h.read_source("a".into(), "user-source".into())
            .await
            .unwrap()
            .is_some()
    );
    h.grant_sources("a".into(), vec![]).await.unwrap();
    assert_eq!(
        h.read_source("a".into(), "user-source".into())
            .await
            .unwrap(),
        None
    );
    assert!(
        h.read_source("a".into(), "own-failed".into())
            .await
            .unwrap()
            .is_some()
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}
#[tokio::test]
async fn shared_work_names_dependencies_grants_and_revocation_are_checked_without_mcp() {
    let (h, t) = start().await;
    submit(&h, "user", ":calc { return 41; } > user_value").await;
    let a = h
        .submit(actor("a-root", "a", "catalog echo value:one > root"))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    denied(
        h.submit(actor("overwrite", "a", ":calc { return 0; } > user_value"))
            .await,
    );
    let foreign = h
        .submit(actor("foreign", "b", ":refresh $root"))
        .await
        .unwrap_err();
    let reason = foreign.authority_message().unwrap();
    assert!(
        reason.contains("target is protected work") && reason.contains("host controls"),
        "{reason}"
    );
    assert!(!reason.contains("root"));
    assert!(
        h.submit(actor("own", "a", ":refresh $root"))
            .await
            .unwrap()
            .diagnostics
            .diagnostics
            .is_empty()
    );
    h.wait_idle().await.unwrap();
    h.submit(actor(
        "b-child",
        "b",
        ":calc { return $root; } > shared_report",
    ))
    .await
    .unwrap();
    h.wait_idle().await.unwrap();
    denied(
        h.submit(actor(
            "dependent",
            "a",
            ":node remove $root scope:downstream",
        ))
        .await,
    );
    assert!(matches!(
        h.cancel_actor("a".into(), a.nodes.clone()).await,
        Err(SessionError::AccessDenied(message)) if message.contains("protected work")
            && message.contains("downstream") && message.contains("host controls") && !message.contains("shared_report")
    ));
    assert!(matches!(
        h.cancel_actor_work("a".into(), "a-root".into()).await,
        Err(SessionError::AccessDenied(message)) if message.contains("protected work") && message.contains("downstream")
    ));
    h.grant_work("a".into(), vec!["b-child".into()])
        .await
        .unwrap();
    // Grant is exact, not workspace-wide or a transitive authority upgrade.
    denied(
        h.submit(actor(
            "unrelated",
            "a",
            ":node remove $user_value scope:downstream",
        ))
        .await,
    );
    h.submit(actor("granted-refresh", "a", ":refresh $root"))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    h.grant_work("a".into(), vec![]).await.unwrap();
    denied(
        h.submit(actor("revoked", "a", ":refresh $shared_report"))
            .await,
    );
    // A plain alias is protected work even though it creates no graph node.
    submit(&h, "user-alias", "$root > pinned_by_user").await;
    h.grant_work("a".into(), vec!["b-child".into()])
        .await
        .unwrap();
    denied(
        h.submit(actor(
            "alias-protected",
            "a",
            ":node remove $root scope:downstream",
        ))
        .await,
    );
    let alias = h
        .submit(actor("alias-refresh", "a", ":refresh $root"))
        .await
        .unwrap_err();
    let reason = alias.authority_message().unwrap();
    assert!(
        reason.contains("result name or alias") && reason.contains("host controls"),
        "{reason}"
    );
    assert!(!reason.contains("pinned_by_user"));
    let snap = h.snapshot().await.unwrap();
    assert_eq!(
        snap.execution.values[&snap.names["user_value"].node].data(),
        &Data::Int(41)
    );
    assert!(snap.names.contains_key("root") && snap.names.contains_key("pinned_by_user"));
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}
#[tokio::test]
async fn reactive_batch_does_not_change_user_defaults_and_partial_admission_keeps_protection() {
    let (h, t) = start().await;
    let first = h
        .submit(
            actor(
                "batch",
                "a",
                "catalog echo value:first > root\n:calc { return $root; } > derived",
            )
            .with_reactive(true),
        )
        .await
        .unwrap();
    assert_eq!(first.nodes.len(), 2);
    assert!(
        first.diagnostics.diagnostics.is_empty(),
        "{:?}",
        first.diagnostics
    );
    h.wait_idle().await.unwrap();
    submit(&h, "user-child", ":calc { return $root; } > manual_child").await;
    h.wait_idle().await.unwrap();
    submit(&h, "user-change", ":change $root value:second").await;
    h.wait_idle().await.unwrap();
    let snap = h.snapshot().await.unwrap();
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["derived"].node)
            .unwrap()
            .state(),
        NodeState::Ready
    );
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["manual_child"].node)
            .unwrap()
            .state(),
        NodeState::Stale
    );
    denied(
        h.submit(actor("global", "a", ":workspace policy mode:reactive"))
            .await,
    );
    let batch=h.submit(actor("partial","b",":calc { return 1; } > fresh\n:calc { return 2; } > manual_child\n:calc { return 3; } > last")).await.unwrap();
    assert_eq!(batch.nodes.len(), 2);
    denied(Ok(batch));
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}
#[tokio::test]
async fn concurrent_actors_cannot_both_claim_an_existing_name() {
    let (h, t) = start().await;
    let (a, b) = tokio::join!(
        h.submit(actor("race-a", "a", ":calc { return 1; } > disputed")),
        h.submit(actor("race-b", "b", ":calc { return 2; } > disputed"))
    );
    let replies = [a.unwrap(), b.unwrap()];
    assert_eq!(replies.iter().filter(|r| r.nodes.len() == 1).count(), 1);
    assert_eq!(
        replies
            .iter()
            .filter(|r| r.diagnostics.diagnostics.iter().any(|d| d.code == "AUT001"))
            .count(),
        1
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}

#[tokio::test]
async fn delegated_rebinding_does_not_turn_user_names_into_actor_ownership() {
    let (h, t) = start().await;
    submit(&h, "user", ":calc { return 1; } > user_value").await;
    h.wait_idle().await.unwrap();
    h.grant_work("a".into(), vec!["user".into()]).await.unwrap();
    let reply = h
        .submit(actor("authorized", "a", ":calc { return 2; } > user_value"))
        .await
        .unwrap();
    assert_eq!(reply.nodes.len(), 1);
    h.grant_work("a".into(), vec![]).await.unwrap();
    denied(
        h.submit(actor(
            "after-revoke",
            "a",
            ":calc { return 3; } > user_value",
        ))
        .await,
    );
    h.submit(actor("own-call", "a", "catalog echo value:first > own"))
        .await
        .unwrap();
    h.wait_idle().await.unwrap();
    submit(&h, "user-edit", ":change $own value:user-edit").await;
    h.wait_idle().await.unwrap();
    denied(
        h.submit(actor(
            "clobber-user-edit",
            "a",
            ":change $own value:clobber",
        ))
        .await,
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}

#[tokio::test]
async fn calculation_definitions_cannot_replace_shared_definitions() {
    let (h, t) = start().await;
    let text = include_str!("../../../../examples/list-registries/definitions.wes");
    let first = submit(&h, "user-definition", text).await;
    assert!(
        !first
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        first.diagnostics
    );
    let rejected = h
        .submit(actor("replace-definition", "a", text))
        .await
        .unwrap();
    assert!(
        rejected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}

#[tokio::test]
async fn actor_observation_does_not_generate_its_own_wait_wakeups() {
    let (h, t) = start().await;
    let mut updates = h.subscribe_updates().unwrap();
    for _ in 0..3 {
        h.observe_actor("a".into(), "ui".into()).await.unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), updates.recv())
            .await
            .is_err()
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}

#[tokio::test]
async fn per_submission_policy_replays_without_restoring_agent_authority() {
    use wes_engine::history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint};
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
    let (h, t) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    h.observe_actor("a".into(), "ui".into()).await.unwrap();
    h.submit(
        actor(
            "recorded-batch",
            "a",
            "catalog echo value:one > root\n:calc { return $root; } > derived",
        )
        .with_reactive(true),
    )
    .await
    .unwrap();
    h.wait_idle().await.unwrap();
    h.grant_sources("a".into(), vec!["recorded-batch".into()])
        .await
        .unwrap();
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let entries = records.lock().unwrap().clone();
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for record in &entries {
        capture.push(record.clone()).unwrap();
    }
    let image = capture.finish(HistoryCheckpoint {
        journal: AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: entries.len() as u64,
        },
        recovery: AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: entries.len() as u64,
        },
    });
    let (base, _) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (h, t) = restored
        .spawn(no_files(), NonZeroUsize::new(2).unwrap())
        .unwrap();
    h.observe_actor("a".into(), "ui".into()).await.unwrap();
    denied(h.submit(actor("old-identity", "a", ":refresh $root")).await);
    assert_eq!(
        h.read_source("a".into(), "recorded-batch".into())
            .await
            .unwrap(),
        None,
        "neither old authorship nor a source grant is restored as live authority"
    );
    submit(&h, "user-refresh", ":refresh $root").await;
    h.wait_idle().await.unwrap();
    submit(&h, "user-change", ":change $root value:two").await;
    h.wait_idle().await.unwrap();
    let snap = h.snapshot().await.unwrap();
    assert_eq!(
        snap.execution
            .graph
            .node(&snap.names["derived"].node)
            .unwrap()
            .state(),
        NodeState::Ready
    );
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}

#[tokio::test]
async fn cooperative_describe_reaches_binding_but_save_and_load_still_require_user_authority() {
    let (h, t) = start().await;
    let reply = h
        .submit(actor(
            "describe-unavailable",
            "a",
            ":describe file:synthetic.json provider:fixture",
        ))
        .await
        .unwrap();
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "DSC001"),
        "{reply:?}"
    );
    for command in [
        r#":workspace save "synthetic""#,
        r#":workspace load "synthetic""#,
    ] {
        denied(h.submit(actor(command, "a", command)).await);
    }
    h.shutdown().await.unwrap();
    t.join().await.unwrap();
}
