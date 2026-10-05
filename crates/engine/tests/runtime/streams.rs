use super::*;

fn running(runtime: &mut Runtime<&'static str>) -> RunTicket<&'static str> {
    tickets(&runtime.start(at(0))).into_iter().next().unwrap()
}
#[test]
fn stream_publication_requires_current_entered_authority_and_keeps_a_physical_lease() {
    let mut runtime = Runtime::new();
    let node = runtime.add("stream", [], SAFE).unwrap();
    let effects = runtime.start(at(0));
    let ticket = tickets(&effects).remove(0);
    let timer = watches(&effects).remove(0);
    assert!(
        runtime
            .stream_window(&ticket.run, value(1), at(0))
            .is_none()
    );
    assert!(runtime.enter_stream(&ticket.run));
    runtime.stream_window(&ticket.run, value(1), at(0)).unwrap();
    assert!(runtime.is_idle());
    assert!(!runtime.is_drained());
    assert!(runtime.is_streaming(&node));
    assert!(runtime.expire(&timer, at(1000)).is_empty());
    runtime.cancel(&node, at(1001));
    assert!(!runtime.is_idle());
    assert!(
        runtime
            .stream_window(&ticket.run, value(99), at(1001))
            .is_none()
    );
    runtime.complete(&ticket.run, Outcome::Produced(value(99)), at(1001));
    assert!(runtime.is_drained());
    assert_eq!(runtime.value_of(&node), None);
    let finite = runtime.add("finite", [], SAFE).unwrap();
    let ticket = running(&mut runtime);
    assert_eq!(ticket.run.node(), &finite);
    assert!(runtime.enter(&ticket.run));
    assert!(!runtime.enter_stream(&ticket.run));
    assert!(
        runtime
            .stream_window(&ticket.run, value(5), at(0))
            .is_none()
    );
}
#[test]
fn explicit_stream_deadline_survives_opening_and_replacement_keeps_start_time() {
    let mut runtime = Runtime::new();
    let node = runtime.add("stream", [], SAFE).unwrap();
    runtime.set_timeout(&node, at(2)).unwrap();
    let effects = runtime.start(at(0));
    let old = watches(&effects).remove(0);
    let ticket = tickets(&effects).remove(0);
    runtime.enter_stream(&ticket.run);
    let opened = runtime.stream_window(&ticket.run, value(1), at(1)).unwrap();
    assert!(opened.iter().any(|e| matches!(
        e,
        Effect::StreamReady {
            deadline: Some(_),
            ..
        }
    )));
    let current = watches(&runtime.set_timeout(&node, at(3)).unwrap()).remove(0);
    assert!(runtime.expire(&old, at(2)).is_empty());
    assert!(runtime.expire(&current, at(2)).is_empty());
    assert!(!runtime.expire(&current, at(3)).is_empty());
    assert_eq!(
        runtime.graph().node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(!runtime.is_drained());
}
#[test]
fn manual_stream_refresh_starts_once_after_old_exit_and_late_windows_cannot_cross_runs() {
    let mut runtime = Runtime::new();
    let node = runtime.add("stream", [], SAFE).unwrap();
    let old = running(&mut runtime);
    runtime.enter_stream(&old.run);
    runtime.stream_window(&old.run, value(1), at(0)).unwrap();
    assert!(tickets(&runtime.refresh(&node, at(1)).unwrap()).is_empty());
    assert!(runtime.refresh(&node, at(1)).unwrap().is_empty());
    assert!(runtime.stream_window(&old.run, value(99), at(1)).is_none());
    let next = tickets(&runtime.complete(&old.run, Outcome::Produced(value(99)), at(2))).remove(0);
    assert_ne!(old.run, next.run);
    assert_eq!(old.run.node(), next.run.node());
    runtime.enter_stream(&next.run);
    runtime.stream_window(&next.run, value(2), at(2)).unwrap();
    assert!(runtime.stream_window(&old.run, value(100), at(3)).is_none());
    assert_eq!(runtime.value_of(&node), Some(&value(2)));
    assert!(
        runtime
            .complete(&old.run, Outcome::Produced(value(100)), at(3))
            .is_empty()
    );
}
#[test]
fn explicit_cancel_withdraws_a_refresh_that_is_waiting_for_stream_cleanup() {
    let mut runtime = Runtime::new();
    let node = runtime.add("stream", [], SAFE).unwrap();
    let old = running(&mut runtime);
    runtime.enter_stream(&old.run);
    runtime.stream_window(&old.run, value(1), at(0)).unwrap();
    runtime.refresh(&node, at(1)).unwrap();
    runtime.cancel(&node, at(1));
    assert!(tickets(&runtime.complete(&old.run, Outcome::Produced(value(99)), at(2))).is_empty());
    assert!(runtime.is_drained());
}
#[test]
fn windows_invalidate_dependents_but_only_repeat_safe_reactive_calls() {
    let mut runtime = Runtime::new();
    let source = runtime.add("stream", [], SAFE).unwrap();
    let safe = runtime
        .add("safe", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    let manual = runtime
        .add("manual", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    let unsafe_ = runtime
        .add("unsafe", [output(&source, OutputPort::Data)], UNSAFE)
        .unwrap();
    runtime.set_policy(&safe, Policy::Reactive).unwrap();
    runtime.set_policy(&unsafe_, Policy::Reactive).unwrap();
    let stream = running(&mut runtime);
    runtime.enter_stream(&stream.run);
    let first = runtime.stream_window(&stream.run, value(1), at(0)).unwrap();
    assert_eq!(tickets(&first).len(), 3);
    for ticket in tickets(&first) {
        runtime.enter(&ticket.run);
        runtime.complete(&ticket.run, Outcome::Produced(value(1)), at(0));
    }
    let updates = runtime.stream_window(&stream.run, value(2), at(1)).unwrap();
    let second = tickets(&updates);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].run.node(), &safe);
    assert_eq!(second[0].inputs.get(&source), Some(&value(2)));
    assert_eq!(
        runtime.graph().node(&manual).unwrap().state(),
        NodeState::Stale
    );
    assert_eq!(
        runtime.graph().node(&unsafe_).unwrap().state(),
        NodeState::Stale
    );
    assert_eq!(
        runtime.stale_reason(&manual),
        Some(StaleReason::StreamUpdated)
    );
    assert_eq!(
        runtime.stale_reason(&unsafe_),
        Some(StaleReason::StreamUpdated)
    );
    assert_eq!(runtime.stale_reason(&safe), None);
    runtime.enter(&second[0].run);
    // Final items that arrived under the cadence still invalidate the currently executing consumer.
    let final_ = runtime.complete(&stream.run, Outcome::Produced(value(3)), at(2));
    assert!(tickets(&final_).is_empty());
    let latest =
        tickets(&runtime.complete(&second[0].run, Outcome::Produced(value(2)), at(2))).remove(0);
    assert_eq!(latest.inputs.get(&source), Some(&value(3)));
}
#[test]
fn stream_terminal_channels_release_never_executed_handlers_and_invalidate_old_data_consumers() {
    for failed in [false, true] {
        let mut runtime = Runtime::new();
        let source = runtime.add("stream", [], SAFE).unwrap();
        let data = runtime
            .add("data", [output(&source, OutputPort::Data)], SAFE)
            .unwrap();
        let handler = runtime
            .add(
                "handler",
                [output(
                    &source,
                    if failed {
                        OutputPort::Error
                    } else {
                        OutputPort::Cancel
                    },
                )],
                SAFE,
            )
            .unwrap();
        let stream = running(&mut runtime);
        runtime.enter_stream(&stream.run);
        let first = runtime.stream_window(&stream.run, value(1), at(0)).unwrap();
        let data_run = tickets(&first).remove(0);
        runtime.enter(&data_run.run);
        runtime.complete(&data_run.run, Outcome::Produced(value(1)), at(0));
        assert_eq!(
            runtime.graph().node(&handler).unwrap().state(),
            NodeState::Skipped
        );
        let effects = if failed {
            runtime.complete(
                &stream.run,
                Outcome::Failed(RuntimeCode::ExecutionFailed.error("fixture", None)),
                at(1),
            )
        } else {
            runtime.cancel(&source, at(1))
        };
        let selected = tickets(&effects);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].run.node(), &handler);
        assert_eq!(
            runtime.graph().node(&data).unwrap().state(),
            if failed {
                NodeState::Failed
            } else {
                NodeState::Skipped
            }
        );
    }
}
#[test]
fn stream_close_and_drop_revoke_publication_until_the_lease_physically_exits() {
    let mut runtime = Runtime::new();
    let node = runtime.add("stream", [], SAFE).unwrap();
    let stream = running(&mut runtime);
    runtime.enter_stream(&stream.run);
    runtime.stream_window(&stream.run, value(1), at(0)).unwrap();
    runtime.stream_closing(&stream.run).unwrap();
    assert!(!runtime.is_idle());
    assert!(
        runtime
            .stream_window(&stream.run, value(2), at(1))
            .is_none()
    );
    runtime.drop_node(&node).unwrap();
    assert!(matches!(
        runtime.add_at(node.clone(), "reuse", [], SAFE),
        Err(RuntimeError::Busy(_))
    ));
    runtime.close();
    assert!(!runtime.is_drained());
    runtime.complete(&stream.run, Outcome::Produced(value(3)), at(1));
    assert!(runtime.is_drained());
    assert!(runtime.value_of(&node).is_none());
}

#[test]
fn a_never_started_dependent_keeps_its_first_run_when_another_input_arrives_later() {
    let mut runtime = Runtime::new();
    let source = runtime.add("stream", [], SAFE).unwrap();
    let other = runtime.add("other", [], SAFE).unwrap();
    let child = runtime
        .add(
            "child",
            [
                output(&source, OutputPort::Data),
                output(&other, OutputPort::Data),
            ],
            SAFE,
        )
        .unwrap();
    let started = tickets(&runtime.start(at(0)));
    let stream = started.iter().find(|t| t.run.node() == &source).unwrap();
    let delayed = started.iter().find(|t| t.run.node() == &other).unwrap();
    runtime.enter_stream(&stream.run);
    runtime.enter(&delayed.run);
    runtime.stream_window(&stream.run, value(1), at(0)).unwrap();
    runtime.stream_window(&stream.run, value(2), at(1)).unwrap();
    assert_eq!(
        runtime.graph().node(&child).unwrap().state(),
        NodeState::Pending
    );
    let first =
        tickets(&runtime.complete(&delayed.run, Outcome::Produced(value(9)), at(1))).remove(0);
    assert_eq!(first.run.node(), &child);
    assert_eq!(first.inputs.get(&source), Some(&value(2)));
}

#[test]
fn later_windows_preserve_an_explicit_dependent_stream_refresh_while_cleanup_is_pending() {
    let mut runtime = Runtime::new();
    let source = runtime.add("stream", [], SAFE).unwrap();
    let child = runtime
        .add("child-stream", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    let parent = running(&mut runtime);
    runtime.enter_stream(&parent.run);
    let child_run =
        tickets(&runtime.stream_window(&parent.run, value(1), at(0)).unwrap()).remove(0);
    runtime.enter_stream(&child_run.run);
    runtime
        .stream_window(&child_run.run, value(10), at(0))
        .unwrap();
    runtime.refresh(&child, at(1)).unwrap();
    runtime.stream_window(&parent.run, value(2), at(1)).unwrap();
    let next =
        tickets(&runtime.complete(&child_run.run, Outcome::Produced(value(99)), at(2))).remove(0);
    assert_eq!(next.run.node(), &child);
    assert_eq!(next.inputs.get(&source), Some(&value(2)));
}

#[test]
fn cancellation_preserves_last_successes_without_reopening_the_data_branch() {
    let mut runtime = Runtime::new();
    runtime.set_default_policy(Policy::Reactive);
    let source = runtime.add("stream", [], SAFE).unwrap();
    let derived = runtime
        .add("derived", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    let stream = running(&mut runtime);
    runtime.enter_stream(&stream.run);
    let first = tickets(&runtime.stream_window(&stream.run, value(1), at(0)).unwrap()).remove(0);
    runtime.enter(&first.run);
    runtime.complete(&first.run, Outcome::Produced(value(10)), at(0));
    let second = tickets(&runtime.stream_window(&stream.run, value(2), at(1)).unwrap()).remove(0);
    runtime.enter(&second.run);
    runtime.cancel(&source, at(2));
    assert_eq!(
        runtime.graph().node(&source).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(
        runtime.graph().node(&derived).unwrap().state(),
        NodeState::Skipped
    );
    assert!(runtime.value_of(&source).is_none());
    assert!(runtime.value_of(&derived).is_none());
    assert_eq!(runtime.stopped_value(&source).unwrap().value, value(2));
    let last = runtime.stopped_value(&derived).unwrap();
    assert_eq!(last.value, value(10));
    assert_eq!(&last.run, first.run.id());
    assert_eq!(last.source, source);
    runtime.complete(&second.run, Outcome::Produced(value(999)), at(3));
    runtime.complete(&stream.run, Outcome::Produced(value(999)), at(3));
    assert_eq!(runtime.stopped_value(&derived).unwrap().value, value(10));
    let new = runtime
        .add("new input", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    assert!(tickets(&runtime.start(at(3))).is_empty());
    assert_eq!(
        runtime.graph().node(&new).unwrap().state(),
        NodeState::Skipped
    );
    assert!(runtime.stopped_value(&new).is_none());
    runtime.refresh(&source, at(4)).unwrap();
    assert!(runtime.stopped_value(&source).is_none());
    assert!(runtime.stopped_value(&derived).is_none());
}

#[test]
fn stopped_display_observations_follow_forget_drop_and_private_lifetime() {
    use wes_core::flow::FlowPolicy;
    for private in [false, true] {
        let mut runtime = Runtime::new();
        let node = runtime.add("stream", [], SAFE).unwrap();
        let stream = running(&mut runtime);
        runtime.enter_stream(&stream.run);
        let v = value(1);
        let v = if private {
            v.with_provenance(Provenance::default().with_policy(&FlowPolicy::default().private()))
        } else {
            v
        };
        runtime.stream_window(&stream.run, v, at(0));
        runtime.cancel(&node, at(1));
        assert_eq!(runtime.stopped_value(&node).is_some(), !private);
        let effects = runtime.forget(&node);
        assert!(runtime.stopped_value(&node).is_none());
        if !private {
            assert!(
                effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Observe(o) if o.stopped.is_none()))
            );
        }
        runtime.drop_node(&node).unwrap();
        assert!(runtime.stopped_value(&node).is_none());
    }
}

#[test]
fn group_cancel_stops_open_sources_but_preserves_finished_finite_work() {
    let mut runtime = Runtime::new();
    runtime.set_default_policy(Policy::Reactive);
    let source = runtime.add("stream", [], SAFE).unwrap();
    let derived = runtime
        .add("derived", [output(&source, OutputPort::Data)], SAFE)
        .unwrap();
    let stream = running(&mut runtime);
    runtime.enter_stream(&stream.run);
    let first = tickets(&runtime.stream_window(&stream.run, value(1), at(0)).unwrap()).remove(0);
    runtime.enter(&first.run);
    runtime.complete(&first.run, Outcome::Produced(value(10)), at(0));
    let finite = runtime.add("finished", [], SAFE).unwrap();
    let finite_run = running(&mut runtime);
    runtime.enter(&finite_run.run);
    runtime.complete(&finite_run.run, Outcome::Produced(value(7)), at(0));
    let effects = runtime
        .cancel_group(&[source.clone(), finite.clone()], at(1))
        .unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Cancel(r) if r == &stream.run))
    );
    assert!(!runtime.is_streaming(&source));
    assert_eq!(
        runtime.graph().node(&source).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(
        runtime.graph().node(&derived).unwrap().state(),
        NodeState::Skipped
    );
    assert_eq!(runtime.stopped_value(&derived).unwrap().value, value(10));
    assert_eq!(runtime.value_of(&finite), Some(&value(7)));
    assert!(runtime.stopped_value(&finite).is_none());
}

#[test]
fn automatic_stream_windows_coalesce_without_cancelling_finite_pure_work() {
    let mut rt = Runtime::new();
    let source = root(&mut rt);
    let child = rt
        .add(
            "pure",
            [OutputRef::data(source.clone())],
            ExecutionTraits { pure: true, ..SAFE },
        )
        .unwrap();
    let tail = rt
        .add("tail", [OutputRef::data(child.clone())], SAFE)
        .unwrap();
    let stream = running(&mut rt);
    assert!(rt.enter_stream(&stream.run));
    let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
    assert!(rt.enter(&first.run));
    for n in 2..100 {
        let effects = rt
            .stream_window(&stream.run, value(n), at(n as u64))
            .unwrap();
        assert!(tickets(&effects).is_empty());
        assert!(!effects.iter().any(|e| matches!(e, Effect::Cancel(_))));
        assert_eq!(state(&rt, &child), NodeState::Running);
        assert!(rt.input_update_pending(&child));
    }
    let effects = rt.complete(&first.run, Outcome::Produced(value(10)), at(100));
    let next = tickets(&effects);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].run.node(), &child);
    assert_eq!(next[0].inputs[&source], value(99));
    assert_eq!(rt.value_of(&child), Some(&value(10)));
    assert!(rt.enter(&next[0].run));
    let effects = rt.complete(&next[0].run, Outcome::Produced(value(990)), at(101));
    let final_ = tickets(&effects);
    assert_eq!(final_.len(), 1);
    assert_eq!(final_[0].run.node(), &tail);
    assert_eq!(final_[0].inputs[&child], value(990));
}

#[test]
fn definition_pause_survives_stream_windows_and_policy_changes_until_explicit_refresh() {
    for before_edit in [Policy::Manual, Policy::Automatic] {
        let mut rt = Runtime::new();
        let source = root(&mut rt);
        let child = rt
            .add(
                "pure",
                [OutputRef::data(source.clone())],
                ExecutionTraits { pure: true, ..SAFE },
            )
            .unwrap();
        let stream = running(&mut rt);
        rt.enter_stream(&stream.run);
        let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
        rt.enter(&first.run);
        rt.complete(&first.run, Outcome::Produced(value(10)), at(2));
        rt.set_policy(&child, before_edit).unwrap();
        rt.invalidate(&child, at(3)).unwrap();
        let effects = rt.stream_window(&stream.run, value(2), at(4)).unwrap();
        assert!(tickets(&effects).is_empty());
        rt.set_policy(&child, Policy::Automatic).unwrap();
        assert!(tickets(&rt.start(at(5))).is_empty());
        assert!(tickets(&rt.stream_window(&stream.run, value(3), at(6)).unwrap()).is_empty());
        assert_eq!(
            rt.stale_reason(&child),
            Some(StaleReason::DefinitionChanged)
        );
        assert_eq!(tickets(&rt.refresh(&child, at(7)).unwrap()).len(), 1);
    }
}

#[test]
fn pending_window_does_not_override_explicit_cancel_or_stream_availability_loss() {
    for cancel_child in [false, true] {
        let mut rt = Runtime::new();
        let source = root(&mut rt);
        let child = rt
            .add(
                "pure",
                [OutputRef::data(source.clone())],
                ExecutionTraits { pure: true, ..SAFE },
            )
            .unwrap();
        let stream = running(&mut rt);
        rt.enter_stream(&stream.run);
        let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
        rt.enter(&first.run);
        rt.stream_window(&stream.run, value(2), at(2)).unwrap();
        let effects = rt.cancel(if cancel_child { &child } else { &source }, at(3));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Cancel(r) if r == &first.run))
        );
        assert!(tickets(&rt.complete(&first.run, Outcome::Produced(value(10)), at(4))).is_empty());
        assert!(rt.output(&OutputRef::data(child)).eq(&OutputState::Closed));
    }
}

#[test]
fn coalesced_chain_waits_for_current_upstream_and_never_delivers_an_old_join() {
    let mut rt = Runtime::new();
    let source = root(&mut rt);
    let pure = ExecutionTraits { pure: true, ..SAFE };
    let child = rt
        .add("child", [OutputRef::data(source.clone())], pure)
        .unwrap();
    let tail = rt
        .add(
            "tail",
            [
                OutputRef::data(child.clone()),
                OutputRef::data(source.clone()),
            ],
            pure,
        )
        .unwrap();
    let stream = running(&mut rt);
    rt.enter_stream(&stream.run);
    let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
    rt.enter(&first.run);
    let old_tail = tickets(&rt.complete(&first.run, Outcome::Produced(value(10)), at(2))).remove(0);
    rt.enter(&old_tail.run);
    let new_child = tickets(&rt.stream_window(&stream.run, value(2), at(3)).unwrap()).remove(0);
    rt.enter(&new_child.run);
    assert!(tickets(&rt.complete(&old_tail.run, Outcome::Produced(value(11)), at(4))).is_empty());
    assert_eq!(state(&rt, &tail), NodeState::Stale);
    let next = tickets(&rt.complete(&new_child.run, Outcome::Produced(value(20)), at(5)));
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].inputs[&child], value(20));
    assert_eq!(next[0].inputs[&source], value(2));
}

#[test]
fn a_failed_coalesced_attempt_is_visible_and_is_not_retried_without_another_window() {
    let mut rt = Runtime::new();
    let source = root(&mut rt);
    let child = rt
        .add(
            "pure",
            [OutputRef::data(source.clone())],
            ExecutionTraits { pure: true, ..SAFE },
        )
        .unwrap();
    let stream = running(&mut rt);
    rt.enter_stream(&stream.run);
    let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
    rt.enter(&first.run);
    rt.stream_window(&stream.run, value(2), at(2)).unwrap();
    let failure = RuntimeCode::ExecutionFailed.error("synthetic failure", None);
    let effects = rt.complete(&first.run, Outcome::Failed(failure.clone()), at(3));
    assert!(tickets(&effects).is_empty());
    assert_eq!(state(&rt, &child), NodeState::Failed);
    assert_eq!(rt.error_of(&child), Some(&failure));
    assert!(!rt.input_update_pending(&child));
    assert!(tickets(&rt.start(at(4))).is_empty());
    assert_eq!(
        tickets(&rt.stream_window(&stream.run, value(3), at(5)).unwrap()).len(),
        1
    );
}

#[test]
fn an_automatic_pause_survives_a_closed_input_and_later_reopening_of_stream_data() {
    let mut rt = Runtime::new();
    let source = root(&mut rt);
    let child = rt
        .add(
            "pure",
            [OutputRef::data(source.clone())],
            ExecutionTraits { pure: true, ..SAFE },
        )
        .unwrap();
    let stream = running(&mut rt);
    rt.enter_stream(&stream.run);
    let first = tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).remove(0);
    rt.enter(&first.run);
    rt.complete(&first.run, Outcome::Produced(value(10)), at(2));
    rt.set_policy(&child, Policy::Manual).unwrap();
    rt.invalidate(&child, at(3)).unwrap();
    rt.cancel(&source, at(4));
    assert_eq!(state(&rt, &child), NodeState::Skipped);
    rt.complete(&stream.run, Outcome::Produced(value(1)), at(5));
    rt.set_policy(&child, Policy::Automatic).unwrap();
    let restarted = tickets(&rt.refresh(&source, at(6)).unwrap()).remove(0);
    rt.enter_stream(&restarted.run);
    assert!(tickets(&rt.stream_window(&restarted.run, value(2), at(7)).unwrap()).is_empty());
    assert!(tickets(&rt.stream_window(&restarted.run, value(3), at(8)).unwrap()).is_empty());
    assert_eq!(
        rt.stale_reason(&child),
        Some(StaleReason::DefinitionChanged)
    );
    assert_eq!(tickets(&rt.refresh(&child, at(9)).unwrap()).len(), 1);
}

#[test]
fn restored_pause_reasons_survive_committed_stream_updates_until_explicit_refresh() {
    for reason in [
        StaleReason::RestoreChanged,
        StaleReason::RestoreNotRetained,
        StaleReason::RestoreUnavailable,
        StaleReason::RestoreUnfinished,
    ] {
        let mut rt = Runtime::new();
        let source = root(&mut rt);
        let child = NodeId::new("held-transform").unwrap();
        rt.restore(
            child.clone(),
            "pure",
            [OutputRef::data(source.clone())],
            ExecutionTraits { pure: true, ..SAFE },
            RestoredState::StaleBecause(reason),
            Some(RunId::new("old-run").unwrap()),
        )
        .unwrap();
        let stream = running(&mut rt);
        rt.enter_stream(&stream.run);
        assert!(tickets(&rt.stream_window(&stream.run, value(1), at(1)).unwrap()).is_empty());
        assert!(tickets(&rt.stream_window(&stream.run, value(2), at(2)).unwrap()).is_empty());
        assert_eq!(rt.stale_reason(&child), Some(reason));
        assert_eq!(tickets(&rt.refresh(&child, at(3)).unwrap()).len(), 1);
    }
}
