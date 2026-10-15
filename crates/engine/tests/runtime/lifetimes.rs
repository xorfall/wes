use super::*;

#[test]
fn joined_result_releases_logical_completion_but_cancellation_only_closes_cleanup() {
    let mut runtime = Runtime::new();
    let node = runtime.add("joined", [], UNSAFE).unwrap();
    let start = runtime.start(at(0));
    let ticket = tickets(&start).remove(0);
    let deadline = watches(&start).remove(0);
    assert!(
        runtime
            .publish_result(&ticket.run, Outcome::Produced(value(1)), at(0))
            .is_none()
    );
    assert!(runtime.enter_joined(&ticket.run));
    let effects = runtime
        .publish_result(&ticket.run, Outcome::Produced(value(7)), at(1))
        .unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ResultPublished { run, .. } if run == &ticket.run))
    );
    assert_eq!(runtime.value_of(&node), Some(&value(7)));
    assert!(runtime.is_idle());
    assert!(!runtime.is_drained());
    assert!(runtime.is_executing(&node));
    assert!(!runtime.is_streaming(&node));
    assert!(runtime.expire(&deadline, at(10000)).is_empty());
    assert!(matches!(
        runtime.refresh(&node, at(2)),
        Err(RuntimeError::Busy(_))
    ));
    assert!(
        runtime
            .publish_result(&ticket.run, Outcome::Produced(value(99)), at(2))
            .is_none()
    );
    let stop = runtime.cancel(&node, at(3));
    assert!(
        stop.iter()
            .any(|e| matches!(e, Effect::Cancel(run) if run == &ticket.run))
    );
    assert!(!runtime.is_idle());
    assert_eq!(
        runtime.graph().node(&node).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(runtime.value_of(&node), Some(&value(7)));
    let effects = runtime.complete(
        &ticket.run,
        Outcome::Cancelled(RuntimeCode::Cancelled.error("cleanup", None)),
        at(4),
    );
    assert!(
        observations(&effects).is_empty(),
        "no duplicate terminal result"
    );
    assert!(runtime.is_drained());
    assert_eq!(runtime.value_of(&node), Some(&value(7)));
    assert_eq!(runtime.run_of(&node), Some(ticket.run.id()));
}

#[test]
fn joined_errors_unknown_and_partial_evidence_remain_ordinary_terminal_ports() {
    for mode in ["failed", "unknown", "partial", "skipped"] {
        let mut runtime = Runtime::new();
        let node = runtime.add("joined", [], UNSAFE).unwrap();
        let error_node = runtime
            .add("handler", [output(&node, OutputPort::Error)], SAFE)
            .unwrap();
        let ticket = tickets(&runtime.start(at(0))).remove(0);
        assert!(runtime.enter_joined(&ticket.run));
        let error = RuntimeCode::ExecutionFailed.error("synthetic", None);
        let outcome = match mode {
            "partial" => Outcome::Incomplete {
                value: value(5),
                error,
            },
            "unknown" => Outcome::Failed(
                wes_core::ErrorValue::new(
                    wes_core::ErrorId::new("unknown").unwrap(),
                    wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
                    "Synthetic unknown",
                    vec![],
                    None,
                )
                .unwrap(),
            ),
            "skipped" => Outcome::Skipped,
            _ => Outcome::Failed(error),
        };
        let effects = runtime.publish_result(&ticket.run, outcome, at(1)).unwrap();
        assert!(runtime.value_of(&node).is_none());
        if mode == "skipped" {
            assert!(tickets(&effects).is_empty());
        } else {
            assert_eq!(tickets(&effects)[0].run.node(), &error_node);
            assert!(matches!(
                runtime.output(&output(&node, OutputPort::Error)),
                OutputState::Available(_)
            ));
        }
        if mode == "partial" {
            assert_eq!(runtime.evidence_value(&node).unwrap().value, value(5));
        }
        let state = runtime.graph().node(&node).unwrap().state();
        runtime.complete(&ticket.run, Outcome::Produced(value(99)), at(2));
        assert_eq!(runtime.graph().node(&node).unwrap().state(), state);
        assert!(runtime.value_of(&node).is_none());
        if mode == "unknown" {
            assert_eq!(
                runtime.error_of(&node).unwrap().code(),
                wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
            );
        }
    }
}

#[test]
fn joined_publication_rejects_ordinary_or_obsolete_work_and_drop_retains_physical_join() {
    let mut runtime = Runtime::new();
    let node = runtime.add("ordinary", [], UNSAFE).unwrap();
    let ticket = tickets(&runtime.start(at(0))).remove(0);
    assert!(runtime.enter(&ticket.run));
    assert!(
        runtime
            .publish_result(&ticket.run, Outcome::Produced(value(1)), at(1))
            .is_none()
    );
    runtime.complete(&ticket.run, Outcome::Produced(value(1)), at(1));
    let next = tickets(&runtime.refresh(&node, at(2)).unwrap()).remove(0);
    assert!(runtime.enter_joined(&next.run));
    assert!(
        runtime
            .publish_result(&ticket.run, Outcome::Produced(value(99)), at(2))
            .is_none()
    );
    runtime
        .publish_result(&next.run, Outcome::Produced(value(2)), at(3))
        .unwrap();
    let (removed, effects) = runtime.drop_node(&node).unwrap();
    assert!(removed.contains(&node));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Cancel(run) if run == &next.run))
    );
    assert!(!runtime.is_drained());
    runtime.complete(&next.run, Outcome::Produced(value(99)), at(4));
    assert!(runtime.is_drained());
    assert!(runtime.graph().node(&node).is_none());
}

#[test]
fn acknowledged_lifetime_value_is_usable_but_never_a_source_or_completed_owner() {
    let mut runtime = Runtime::new();
    let node = runtime.add("recording", [], UNSAFE).unwrap();
    let effects = runtime.start(at(0));
    let ticket = tickets(&effects).remove(0);
    let opening_deadline = watches(&effects).remove(0);
    assert!(
        runtime
            .lifetime_value(&ticket.run, value(1), at(0))
            .is_none()
    );
    assert!(runtime.enter_lifetime(&ticket.run));
    assert!(
        runtime
            .stream_window(&ticket.run, value(99), at(0))
            .is_none()
    );
    let effects = runtime
        .lifetime_value(&ticket.run, value(1), at(1))
        .unwrap();
    assert!(effects.iter().any(|effect| matches!(effect,
        Effect::LifetimeReady { run, deadline: None } if run == &ticket.run)));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::StreamReady { .. }))
    );
    assert_eq!(
        runtime.graph().node(&node).unwrap().state(),
        NodeState::Ready
    );
    assert!(runtime.is_idle());
    assert!(!runtime.is_drained());
    assert!(runtime.is_executing(&node));
    assert!(!runtime.is_streaming(&node));
    assert!(matches!(
        runtime.output(&OutputRef::data(node.clone())),
        OutputState::Available(_)
    ));
    assert!(runtime.expire(&opening_deadline, at(1000)).is_empty());
    let original_run = runtime.run_of(&node).unwrap().clone();
    runtime.complete(&ticket.run, Outcome::Produced(value(2)), at(1001));
    assert!(runtime.is_drained());
    assert!(!runtime.is_executing(&node));
    assert_eq!(runtime.run_of(&node), Some(&original_run));
    assert_eq!(runtime.value_of(&node), Some(&value(2)));
}

#[test]
fn lifetime_revocation_preserves_join_and_blocks_cross_run_or_source_publication() {
    let mut runtime = Runtime::new();
    let node = runtime.add("recording", [], SAFE).unwrap();
    let old = tickets(&runtime.start(at(0))).remove(0);
    assert!(runtime.enter_lifetime(&old.run));
    runtime.lifetime_value(&old.run, value(1), at(0)).unwrap();
    runtime.cancel(&node, at(1));
    assert!(!runtime.is_drained());
    assert!(!runtime.is_idle());
    assert!(runtime.lifetime_value(&old.run, value(99), at(1)).is_none());
    runtime.complete(&old.run, Outcome::Produced(value(99)), at(2));
    assert!(runtime.is_drained());
    assert!(runtime.value_of(&node).is_none());
    let fresh = tickets(&runtime.refresh(&node, at(3)).unwrap()).remove(0);
    assert!(runtime.enter_lifetime(&fresh.run));
    assert!(runtime.lifetime_value(&old.run, value(99), at(3)).is_none());
    runtime.lifetime_value(&fresh.run, value(3), at(3)).unwrap();
    assert_eq!(runtime.value_of(&node), Some(&value(3)));
    let deadline = watches(&runtime.set_timeout(&node, at(4)).unwrap()).remove(0);
    runtime.expire(&deadline, at(7));
    assert_eq!(
        runtime.graph().node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(!runtime.is_drained());
    runtime.complete(&fresh.run, Outcome::Produced(value(99)), at(7));
    assert!(runtime.is_drained());
    assert!(runtime.value_of(&node).is_none());
}
