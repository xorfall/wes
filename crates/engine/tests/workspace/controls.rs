use super::*;
use wes_engine::{
    plan::Input,
    workspace::{ControlApplied, PreparedControl},
};

fn control(workspace: &Workspace, text: &str) -> PreparedControl {
    let Preparation::Meta(meta) = workspace.prepare(&statement(text)).unwrap() else {
        panic!("meta")
    };
    workspace.prepare_control(meta).unwrap()
}
pub(super) fn apply(workspace: &mut Workspace, text: &str) -> ControlApplied {
    let prepared = control(workspace, text);
    workspace.apply_control(prepared, Duration::ZERO).unwrap()
}
pub(super) fn invalid(workspace: &Workspace, text: &str) -> WorkspaceError {
    match workspace.prepare(&statement(text)) {
        Ok(Preparation::Meta(meta)) => workspace.prepare_control(meta).unwrap_err(),
        Err(error) => error,
        _ => panic!("expected invalid control"),
    }
}

#[test]
fn downstream_refresh_rejects_unknown_and_dynamic_scopes_before_mutation() {
    let (mut workspace, calls) = workspace();
    let root = commit(&mut workspace, "catalog echo value:one > root").unwrap();
    for scope in ["self", "force", "all", "downstreams"] {
        assert_eq!(
            code(invalid(
                &workspace,
                &format!(":refresh $root scope:{scope}")
            )),
            "MET015"
        );
    }
    assert_eq!(
        code(invalid(&workspace, ":refresh $root scope:$root")),
        "CHK017"
    );
    for alias in [":force $root", ":refres $root"] {
        let _ = invalid(&workspace, alias);
    }
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Pending
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let control = control(&workspace, ":refresh $root scope:downstream");
    assert!(control.refreshes_downstream());
    assert!(!control.recorded());
}

#[test]
fn downstream_refresh_preflights_streams_and_selected_stream_inputs_without_mutation() {
    struct NeverStream;
    impl wes_engine::streams::StreamingInvoker for NeverStream {
        fn subscribe(
            &self,
            _: Call,
            _: wes_engine::streams::StreamSink,
            _: CancellationToken,
        ) -> wes_engine::streams::StreamFuture {
            panic!("preflight must not enter a stream")
        }
    }
    let (mut workspace, calls) = workspace();
    let mut stream = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    stream.streaming = true;
    workspace
        .register_provider_ports(
            ProviderDescription::new("events", [stream], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
            Some(Arc::new(NeverStream)),
        )
        .unwrap();
    let source = commit(&mut workspace, "events watch > source").unwrap();
    let dependent = commit(&mut workspace, "catalog echo value:$source > dependent").unwrap();
    for name in ["source", "dependent"] {
        let prepared = control(&workspace, &format!(":refresh ${name} scope:downstream"));
        assert_eq!(
            code(
                workspace
                    .apply_control(prepared, Duration::ZERO)
                    .err()
                    .unwrap()
            ),
            "MET015"
        );
        for node in [&source, &dependent] {
            assert_eq!(
                workspace.runtime().graph().node(node).unwrap().state(),
                NodeState::Pending
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn change_preparation_is_pure_and_literals_follow_the_original_signature() {
    let (mut workspace, calls) = workspace();
    let node = commit(&mut workspace, "catalog count amount:2 > total").unwrap();
    let prepared = control(&workspace, ":change $total amount:4");
    assert!(prepared.recorded());
    let old = workspace
        .runtime()
        .graph()
        .node(&node)
        .unwrap()
        .payload()
        .clone();
    let Input::Literal(value) = &old.call().unwrap().invocation().inputs["amount"] else {
        panic!("literal")
    };
    assert_eq!(value.data(), &Data::Int(2));
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Pending
    );
    let applied = workspace.apply_control(prepared, Duration::ZERO).unwrap();
    assert!(
        !applied
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Spawn(_)))
    );
    let changed = workspace.runtime().graph().node(&node).unwrap();
    assert_eq!(changed.state(), NodeState::Stale);
    let Input::Literal(value) = &changed.payload().call().unwrap().invocation().inputs["amount"]
    else {
        panic!("literal")
    };
    assert_eq!(value, &int(4));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let Input::Literal(value) = &old.call().unwrap().invocation().inputs["amount"] else {
        panic!("literal")
    };
    assert_eq!(value.data(), &Data::Int(2)); // Previously captured work stays immutable.
}

#[test]
fn change_revalidates_original_template_contracts_without_partial_mutation() {
    let (mut workspace, _) = workspace();
    let package = workspace
        .prepare_type_package("types: {Small: {base: Int, min: 1, max: 5}}", Span::at(0))
        .unwrap();
    workspace.commit(package).unwrap();
    commit(
        &mut workspace,
        ":def small(amount: Small) as catalog count amount:?amount",
    );
    let node = commit(&mut workspace, "small amount:2 > total").unwrap();
    let error = invalid(&workspace, ":change $total amount:0");
    let WorkspaceError::Rejected {
        diagnostics,
        issues,
    } = error
    else {
        panic!("rejected")
    };
    assert_eq!(diagnostics[0].code, "TYP005");
    assert_eq!(issues[0].path, "/arguments/amount");
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Pending
    );
    // Existing contracts are immutable; an additional wider definition cannot weaken this call.
    assert_eq!(
        code(
            workspace
                .prepare_type_package("types: {Small: {base: Int, min: 1, max: 50}}", Span::at(0))
                .unwrap_err()
        ),
        "TYP004"
    );
    let package = workspace
        .prepare_type_package("types: {Wide: {base: Int, min: 1, max: 50}}", Span::at(0))
        .unwrap();
    workspace.commit(package).unwrap();
    assert_eq!(
        code(invalid(&workspace, ":change $total amount:6")),
        "TYP005"
    );
    apply(&mut workspace, ":change $total amount:4");
    let Input::Literal(value) = &workspace
        .runtime()
        .graph()
        .node(&node)
        .unwrap()
        .payload()
        .call()
        .unwrap()
        .invocation()
        .inputs["amount"]
    else {
        panic!("literal")
    };
    assert_eq!(value.data(), &Data::Int(4));
}

#[test]
fn change_cannot_add_a_dependency_or_change_its_selected_channel() {
    let (mut workspace, _) = workspace();
    let source = commit(&mut workspace, "catalog echo value:one > source").unwrap();
    commit(&mut workspace, "catalog echo value:two > other");
    let target = commit(&mut workspace, "catalog echo value:$source > target").unwrap();
    assert_eq!(
        code(invalid(&workspace, ":change $target value:$other")),
        "MET014"
    );
    assert_eq!(
        code(invalid(&workspace, ":change $target value:$source::error")),
        "MET014"
    );
    apply(&mut workspace, ":change $target value:literal");
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&target)
            .unwrap()
            .dependencies()
            .get(&source),
        Some(&OutputPort::Data)
    );
    assert_eq!(workspace.resolve("source").unwrap().node, source);
}

#[tokio::test]
async fn change_preserves_the_captured_provider_and_override_cautions() {
    let (mut workspace, original) = workspace();
    let node = commit(
        &mut workspace,
        "@unchecked{value} catalog echo value:one locale:en > target",
    )
    .unwrap();
    let replacement = Arc::new(AtomicUsize::new(0));
    workspace
        .register_provider(metadata(), Arc::new(Echo(replacement.clone())))
        .unwrap();
    apply(&mut workspace, ":change $target value:two locale:fr");
    let call = workspace.runtime().graph().node(&node).unwrap().payload();
    assert!(
        call.call()
            .unwrap()
            .invocation()
            .cautions
            .contains("unchecked:value")
    );
    let effects = apply(&mut workspace, ":refresh $target").effects;
    let work = ticket(effects);
    workspace.enter(&work.run);
    let run = work.run.clone();
    let report = TaskExecutor::ephemeral()
        .execute(work, CancellationToken::new())
        .await;
    workspace.complete(&run, report.outcome, Duration::from_secs(1));
    assert_eq!(original.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.load(Ordering::SeqCst), 0);
    assert!(
        workspace
            .data_typing(&node)
            .unwrap()
            .provenance
            .cautions()
            .contains("unchecked:value")
    );
}

#[test]
fn cancellation_and_refresh_do_not_obsolete_staged_declarations_or_overlap_a_lease() {
    let (mut workspace, _) = workspace();
    let node = commit(&mut workspace, "catalog echo value:one > target").unwrap();
    let running = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&running.run);
    let staged = prepare(&workspace, "catalog echo value:two > later");
    let cancellation = control(&workspace, ":cancel $target");
    assert!(!cancellation.recorded());
    let applied = workspace
        .apply_control(cancellation, Duration::from_secs(1))
        .unwrap();
    assert!(
        applied
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(run) if run == &running.run))
    );
    let refresh = control(&workspace, ":refresh $target");
    assert!(matches!(
        workspace.apply_control(refresh, Duration::ZERO),
        Err(WorkspaceError::Runtime(
            wes_engine::runtime::RuntimeError::Busy(_)
        ))
    ));
    workspace.commit(staged).unwrap();
    workspace.complete(
        &running.run,
        Outcome::Produced(int(99)),
        Duration::from_secs(2),
    );
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    let replacement = ticket(apply(&mut workspace, ":refresh $target").effects);
    assert_ne!(running.run, replacement.run);
}

#[test]
fn timeout_controls_validate_before_mutation_and_keep_the_original_start_instant() {
    let (mut workspace, _) = workspace();
    let node = commit(&mut workspace, "catalog echo value:one > target").unwrap();
    for duration in ["PT0S", "-PT0.000000001S", "PT9223372037S"] {
        assert_eq!(
            code(invalid(
                &workspace,
                &format!(":timeout $target after:{duration}")
            )),
            "MET013"
        );
    }
    let running = ticket(workspace.start(Duration::from_secs(10)));
    workspace.enter(&running.run);
    let prepared = control(&workspace, ":timeout $target after:PT5S");
    let applied = workspace
        .apply_control(prepared, Duration::from_secs(20))
        .unwrap();
    let deadline = applied
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Watch(deadline) => Some(deadline),
            _ => None,
        })
        .unwrap();
    assert_eq!(deadline.run(), &running.run);
    assert!(
        workspace
            .expire(&deadline, Duration::from_secs(14))
            .is_empty()
    );
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Running
    );
    assert!(
        !workspace
            .expire(&deadline, Duration::from_secs(15))
            .is_empty()
    );
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Cancelled
    );
}

#[test]
fn policy_changes_invalidate_prepared_declarations_and_only_safe_reactive_changes_start() {
    let (mut workspace, _) = workspace();
    let node = commit(&mut workspace, "catalog echo value:one > target").unwrap();
    let staged = prepare(&workspace, "catalog echo value:two");
    apply(&mut workspace, ":workspace policy mode:reactive");
    assert!(matches!(
        workspace.commit(staged),
        Err(WorkspaceError::Obsolete)
    ));
    let work = ticket(apply(&mut workspace, ":change $target value:two").effects);
    assert_eq!(work.run.node(), &node);
    assert_eq!(
        code(invalid(&workspace, ":workspace policy mode:eventually")),
        "MET005"
    );
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Unsafe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("risky", [capability], vec![]).unwrap(),
            Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
        )
        .unwrap();
    let unsafe_node = commit(&mut workspace, "risky echo value:one > unsafeCall").unwrap();
    assert!(
        !apply(&mut workspace, ":change $unsafeCall value:two")
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Spawn(_)))
    );
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&unsafe_node)
            .unwrap()
            .state(),
        NodeState::Stale
    );
}

#[test]
fn dropping_a_binding_keeps_nodes_and_edges_while_dropping_a_node_removes_dependents() {
    let (mut workspace, _) = workspace();
    let source = commit(&mut workspace, "catalog echo value:one > source").unwrap();
    let target = commit(&mut workspace, "catalog echo value:$source > target").unwrap();
    let unbound = apply(&mut workspace, ":name unbind \"source\"");
    assert_eq!(unbound.unbound, ["source"]);
    assert!(unbound.removed.is_empty());
    assert!(workspace.resolve("source").is_none());
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&target)
            .unwrap()
            .dependencies()
            .get(&source),
        Some(&OutputPort::Data)
    );
    assert_eq!(
        code(invalid(&workspace, ":name unbind \"source\"")),
        "MET007"
    );
    let removed = apply(
        &mut workspace,
        &format!(":node remove ${source} scope:downstream"),
    );
    assert_eq!(removed.removed, [source, target]);
    assert_eq!(removed.unbound, ["target"]);
    assert!(workspace.runtime().graph().is_empty());
    assert!(workspace.bindings().names().is_empty());
}

#[test]
fn control_stamps_reject_foreign_workspaces_and_obsolete_declarations() {
    let (mut workspace, _) = workspace();
    commit(&mut workspace, "catalog echo value:one > target");
    let (mut foreign, _) = super::workspace();
    let prepared = control(&workspace, ":workspace policy mode:reactive");
    assert!(matches!(
        foreign.apply_control(prepared, Duration::ZERO),
        Err(WorkspaceError::Obsolete)
    ));
    let prepared = control(&workspace, ":node remove $target scope:downstream");
    commit(&mut workspace, "$target > alias");
    assert!(matches!(
        workspace.apply_control(prepared, Duration::ZERO),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(workspace.resolve("target").is_some());
}

#[test]
fn ambiguous_drop_and_literal_policy_subjects_do_not_mutate_the_workspace() {
    let (mut workspace, _) = workspace();
    let target = commit(&mut workspace, "catalog echo value:one > target").unwrap();
    assert!(matches!(
        invalid(&workspace, ":name unbind \"target\" $target"),
        WorkspaceError::Rejected { .. }
    ));
    assert!(matches!(
        invalid(&workspace, ":policy \"target\" mode:reactive"),
        WorkspaceError::Rejected { .. }
    ));
    assert_eq!(workspace.resolve("target").unwrap().node, target);
    assert!(
        !apply(&mut workspace, ":change $target value:two")
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Spawn(_)))
    );
}

#[test]
fn prepared_waits_capture_ports_without_becoming_obsolete_when_aliases_change() {
    let (mut workspace, _) = workspace();
    let original = commit(&mut workspace, "catalog echo value:one > chosen").unwrap();
    let other = commit(&mut workspace, "catalog echo value:two > other").unwrap();
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(":wait $chosen::cancel $chosen::cancel"))
        .unwrap()
    else {
        panic!("wait")
    };
    let waiting = workspace.prepare_wait(meta).unwrap();
    assert_eq!(waiting.selected().len(), 1);
    assert_eq!(waiting.budget(), Duration::from_secs(60));
    assert_eq!(waiting.unavailable().code, "MET006");
    commit(&mut workspace, "$other > chosen");
    let output = waiting.selected().outputs().next().unwrap();
    assert_eq!(output.node, original);
    assert_eq!(output.port, OutputPort::Cancel);
    assert_eq!(workspace.resolve("chosen").unwrap().node, other);
    assert_eq!(
        workspace.runtime().selected_outputs(waiting.selected()),
        None
    );
    workspace.drop_node(&original).unwrap();
    assert_eq!(
        workspace.runtime().selected_outputs(waiting.selected()),
        Some(false)
    );
}

#[test]
fn wait_rejects_conflicting_ports_and_literal_subjects() {
    let (mut workspace, _) = workspace();
    commit(&mut workspace, "catalog echo value:one > target");
    for text in [":wait $target $target::error", ":wait \"target\""] {
        let Preparation::Meta(meta) = workspace.prepare(&statement(text)).unwrap() else {
            panic!("wait")
        };
        assert_eq!(code(workspace.prepare_wait(meta).unwrap_err()), "MET006");
    }
}

#[tokio::test]
async fn control_receipts_name_actual_targets_and_distinguish_requests_from_completion() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog echo value:one > producer").unwrap();
    let child = commit(&mut workspace, "catalog echo value:$producer > consumer").unwrap();
    let mut effects = std::collections::VecDeque::from(workspace.start(Duration::ZERO));
    while let Some(effect) = effects.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            assert!(workspace.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
        }
    }
    let change = apply(&mut workspace, ":change $producer value:two");
    assert_eq!(change.receipt.operation, "change");
    assert_eq!(change.receipt.target, "$producer");
    assert!(change.receipt.stale >= 1);
    assert!(change.receipt.summary().contains("marked stale"));
    assert!(
        workspace
            .runtime()
            .graph()
            .node(&root)
            .unwrap()
            .payload()
            .call()
            .unwrap()
            .definition_changed()
    );
    let refreshed = apply(&mut workspace, ":refresh $producer");
    assert_eq!(refreshed.receipt.requested, 1);
    assert!(
        refreshed
            .receipt
            .summary()
            .contains("1 execution requested")
    );
    assert!(!refreshed.receipt.summary().contains("completed"));
    let mut effects = std::collections::VecDeque::from(refreshed.effects);
    while let Some(effect) = effects.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            assert!(workspace.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
        }
    }
    let downstream = apply(&mut workspace, ":refresh $producer scope:downstream");
    assert_eq!(downstream.receipt.requested, 2);
    assert!(
        downstream
            .receipt
            .summary()
            .contains("2 executions requested")
    );
    let removed = apply(&mut workspace, ":remove $producer scope:downstream");
    assert_eq!(removed.receipt.removed, 2);
    assert_eq!(removed.receipt.unbound, 2);
    assert!(workspace.runtime().graph().node(&child).is_none());
}
