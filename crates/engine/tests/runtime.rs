use std::time::Duration;
use wes_core::{Data, Primitive, Provenance, Shape, Value};
use wes_engine::{
    graph::{NodeId, NodeState, OutputPort, OutputRef},
    runtime::*,
};

const SAFE: ExecutionTraits = ExecutionTraits {
    pure: false,
    repeatable: true,
    bounded: true,
};
const UNSAFE: ExecutionTraits = ExecutionTraits {
    pure: false,
    repeatable: false,
    bounded: true,
};
fn at(second: u64) -> Duration {
    Duration::from_secs(second)
}
fn value(number: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(number),
        Provenance::default(),
    )
    .unwrap()
}
fn output(node: &NodeId, port: OutputPort) -> OutputRef {
    OutputRef {
        node: node.clone(),
        port,
    }
}
fn tickets(effects: &[Effect<&'static str>]) -> Vec<RunTicket<&'static str>> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket.clone())
            } else {
                None
            }
        })
        .collect()
}
fn watches(effects: &[Effect<&'static str>]) -> Vec<Deadline> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Watch(deadline) = effect {
                Some(deadline.clone())
            } else {
                None
            }
        })
        .collect()
}
fn observations(effects: &[Effect<&'static str>]) -> Vec<Observation> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Observe(observation) = effect {
                Some(observation.clone())
            } else {
                None
            }
        })
        .collect()
}
fn state(runtime: &Runtime<&'static str>, node: &NodeId) -> NodeState {
    runtime.graph().node(node).unwrap().state()
}
fn root(runtime: &mut Runtime<&'static str>) -> NodeId {
    runtime.add("root", [], SAFE).unwrap()
}

#[test]
fn automatic_recomputes_only_verified_bounded_pure_dependents_and_manual_wins() {
    let mut rt = Runtime::new();
    let source = root(&mut rt);
    let pure = ExecutionTraits { pure: true, ..SAFE };
    let automatic = rt
        .add("automatic", [OutputRef::data(source.clone())], pure)
        .unwrap();
    let manual = rt
        .add("manual", [OutputRef::data(source.clone())], pure)
        .unwrap();
    rt.set_policy(&manual, Policy::Manual).unwrap();
    let external = rt
        .add("safe external", [OutputRef::data(source.clone())], SAFE)
        .unwrap();
    let unbounded = rt
        .add(
            "unbounded",
            [OutputRef::data(source.clone())],
            ExecutionTraits {
                bounded: false,
                ..pure
            },
        )
        .unwrap();
    let source_run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&source_run));
    let first = tickets(&rt.complete(&source_run, Outcome::Produced(value(1)), at(1)));
    assert_eq!(first.len(), 4);
    for ticket in first {
        assert!(rt.enter(&ticket.run));
        rt.complete(&ticket.run, Outcome::Produced(value(2)), at(2));
    }
    let source_run = tickets(&rt.refresh(&source, at(3)).unwrap())[0].run.clone();
    assert!(rt.enter(&source_run));
    let changed = tickets(&rt.complete(&source_run, Outcome::Produced(value(3)), at(4)));
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].run.node(), &automatic);
    for held in [&manual, &external, &unbounded] {
        assert_eq!(state(&rt, held), NodeState::Stale);
    }
    assert!(rt.enter(&changed[0].run));
    rt.complete(&changed[0].run, Outcome::Produced(value(4)), at(5));
    assert!(tickets(&rt.invalidate(&automatic, at(6)).unwrap()).is_empty());
    assert_eq!(
        rt.stale_reason(&automatic),
        Some(StaleReason::DefinitionChanged)
    );
    assert!(rt.start(at(7)).is_empty());
    let source_run = tickets(&rt.refresh(&source, at(8)).unwrap())[0].run.clone();
    assert!(rt.enter(&source_run));
    assert!(tickets(&rt.complete(&source_run, Outcome::Produced(value(5)), at(9))).is_empty());
    assert_eq!(
        rt.stale_reason(&automatic),
        Some(StaleReason::DefinitionChanged)
    );
    assert_eq!(tickets(&rt.refresh(&automatic, at(10)).unwrap()).len(), 1);
}

#[test]
fn readiness_and_input_snapshot_are_one_transition() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("consumer", [OutputRef::data(a.clone())], SAFE)
        .unwrap();
    let first = tickets(&rt.start(at(0)));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].run.node(), &a);
    assert!(rt.enter(&first[0].run));
    let effects = rt.complete(&first[0].run, Outcome::Produced(value(1)), at(1));
    let next = tickets(&effects);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].run.node(), &b);
    assert_eq!(next[0].inputs[&a], value(1));
    let origin = next[0].input_origins[&a].clone();
    assert_eq!(&origin.run, first[0].run.id());
    assert_eq!(origin.port, OutputPort::Data);
    rt.refresh(&a, at(2)).unwrap();
    assert_eq!(next[0].input_origins[&a].revision, origin.revision);
    assert_eq!(next[0].input_origins[&a].run, origin.run);
    assert_eq!(next[0].inputs[&a], value(1)); // caller owns an immutable captured snapshot
    assert!(!rt.enter(&next[0].run)); // invalidated before entering executor
    assert_eq!(rt.output(&OutputRef::data(a)), OutputState::Pending);
}

#[test]
fn actual_typing_changes_only_with_an_accepted_result_and_survives_eviction() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    assert!(rt.actual_typing(&a).is_none());
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    rt.enter(&run);
    rt.complete(&run, Outcome::Produced(value(1)), at(1));
    let accepted = rt.actual_typing(&a).unwrap().clone();
    assert_eq!(accepted.shape, Shape::Primitive(Primitive::Int));
    let later = tickets(&rt.refresh(&a, at(2)).unwrap())[0].run.clone();
    rt.enter(&later);
    rt.cancel(&a, at(3));
    let unaccepted = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("late".into()),
        Provenance::default(),
    )
    .unwrap();
    rt.complete(&later, Outcome::Produced(unaccepted.clone()), at(4));
    assert!(std::sync::Arc::ptr_eq(
        rt.actual_typing(&a).unwrap(),
        &accepted
    ));
    let latest = tickets(&rt.refresh(&a, at(5)).unwrap())[0].run.clone();
    rt.enter(&latest);
    rt.complete(&latest, Outcome::Produced(unaccepted.clone()), at(6));
    assert_eq!(rt.actual_typing(&a).unwrap().shape, *unaccepted.shape());
    rt.forget(&a);
    assert!(rt.value_of(&a).is_none());
    assert_eq!(rt.actual_typing(&a).unwrap().shape, *unaccepted.shape());
    rt.drop_node(&a).unwrap();
    assert!(rt.actual_typing(&a).is_none());
}

#[test]
fn restored_values_establish_actual_typing_without_starting_work() {
    let mut rt = Runtime::new();
    let node = NodeId::new("id1000").unwrap();
    rt.restore(
        node.clone(),
        "restored",
        [],
        SAFE,
        RestoredState::Ready(value(3)),
        None,
    )
    .unwrap();
    assert_eq!(
        rt.actual_typing(&node).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
    assert!(rt.start(at(0)).is_empty());
}

#[test]
fn staged_admission_is_pending_and_cannot_reuse_a_dropped_live_lease() {
    let mut rt = Runtime::new();
    let candidate = rt.graph().next_id_avoiding(["id1000"]).unwrap();
    rt.add_at(candidate.clone(), "root", [], SAFE).unwrap();
    assert_eq!(state(&rt, &candidate), NodeState::Pending);
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    rt.enter(&run);
    rt.drop_node(&candidate).unwrap();
    assert!(matches!(
        rt.add_at(candidate.clone(), "replacement", [], SAFE),
        Err(RuntimeError::Busy(_))
    ));
    rt.complete(&run, Outcome::Produced(value(8)), at(1));
    rt.add_at(candidate.clone(), "replacement", [], SAFE)
        .unwrap();
    assert!(rt.actual_typing(&candidate).is_none());
    assert_eq!(state(&rt, &candidate), NodeState::Pending);
}

#[test]
fn success_closes_error_and_cancel_branches_without_invoking_them() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("data", [output(&a, OutputPort::Data)], SAFE)
        .unwrap();
    let c = rt
        .add("error", [output(&a, OutputPort::Error)], SAFE)
        .unwrap();
    let d = rt
        .add("cancel", [output(&a, OutputPort::Cancel)], SAFE)
        .unwrap();
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    let effects = rt.complete(&run, Outcome::Produced(value(2)), at(1));
    assert_eq!(
        tickets(&effects)
            .iter()
            .map(|t| t.run.node())
            .collect::<Vec<_>>(),
        [&b]
    );
    assert_eq!(state(&rt, &c), NodeState::Skipped);
    assert_eq!(state(&rt, &d), NodeState::Skipped);
    assert_eq!(
        rt.output(&output(&a, OutputPort::Error)),
        OutputState::Closed
    );
    assert_eq!(
        rt.output(&output(&c, OutputPort::Error)),
        OutputState::Closed
    );
}

#[test]
fn failure_carries_one_identity_and_data_propagation_adds_a_cause() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("data", [output(&a, OutputPort::Data)], SAFE)
        .unwrap();
    let c = rt
        .add("error", [output(&a, OutputPort::Error)], SAFE)
        .unwrap();
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    let failure = RuntimeCode::ExecutionFailed.error("synthetic failure", None);
    let effects = rt.complete(&run, Outcome::Failed(failure.clone()), at(1));
    let observed = observations(&effects);
    assert_eq!(observed[0].error.as_ref(), Some(&failure));
    assert_eq!(rt.error_of(&a), Some(&failure));
    let next = tickets(&effects);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].run.node(), &c);
    assert_eq!(next[0].inputs[&a], failure.to_value());
    let propagated = rt.error_of(&b).unwrap();
    assert_ne!(propagated.id(), failure.id());
    assert_eq!(propagated.cause(), Some(failure.id()));
    assert_eq!(propagated.code(), "RUN004");
}

#[test]
fn handlers_bound_after_completion_select_only_the_available_output() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    rt.cancel(&a, at(1));
    let cancel = rt
        .add("late cancel", [output(&a, OutputPort::Cancel)], SAFE)
        .unwrap();
    let error = rt
        .add("late error", [output(&a, OutputPort::Error)], SAFE)
        .unwrap();
    let data = rt
        .add("late data", [output(&a, OutputPort::Data)], SAFE)
        .unwrap();
    let effects = rt.start(at(2));
    assert_eq!(tickets(&effects)[0].run.node(), &cancel);
    assert_eq!(state(&rt, &error), NodeState::Skipped);
    assert_eq!(state(&rt, &data), NodeState::Skipped);
    assert!(rt.error_of(&data).is_none());
    assert_eq!(rt.error_of(&a).unwrap().code(), "RUN003");
}

#[test]
fn cancellation_revokes_authority_but_lease_survives_until_real_exit() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    rt.cancel(&a, at(1));
    let reason = rt.error_of(&a).unwrap().clone();
    assert!(rt.is_executing(&a));
    assert!(!rt.is_idle());
    assert!(matches!(rt.refresh(&a, at(2)), Err(RuntimeError::Busy(_))));
    assert!(
        rt.complete(&run, Outcome::Produced(value(9)), at(3))
            .is_empty()
    );
    assert_eq!(state(&rt, &a), NodeState::Cancelled);
    assert_eq!(rt.error_of(&a), Some(&reason));
    assert!(rt.is_idle());
    let new = tickets(&rt.refresh(&a, at(4)).unwrap())[0].run.clone();
    assert_ne!(new.id(), run.id());
    assert!(rt.enter(&new));
    assert!(
        rt.complete(&run, Outcome::Produced(value(10)), at(5))
            .is_empty()
    );
    assert!(rt.is_executing(&a));
    assert_eq!(rt.run_of(&a), Some(new.id()));
}

#[test]
fn queued_cancellation_does_not_remove_or_enter_a_new_generation() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let old = tickets(&rt.start(at(0)))[0].run.clone();
    rt.cancel(&a, at(1));
    assert!(rt.is_idle());
    let new = tickets(&rt.refresh(&a, at(2)).unwrap())[0].run.clone();
    assert!(!rt.enter(&old));
    assert!(
        rt.complete(&old, Outcome::Produced(value(99)), at(3))
            .is_empty()
    );
    assert!(rt.enter(&new));
    assert!(!rt.enter(&new));
    assert_eq!(state(&rt, &a), NodeState::Running);
    rt.complete(&new, Outcome::Produced(value(2)), at(4));
    assert_eq!(rt.value_of(&a), Some(&value(2)));
}

#[test]
fn reactive_invalidation_waits_for_obsolete_executor_exit() {
    let mut rt = Runtime::new();
    rt.set_default_policy(Policy::Reactive);
    let a = root(&mut rt);
    let old = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&old));
    assert!(tickets(&rt.invalidate(&a, at(1)).unwrap()).is_empty());
    assert_eq!(state(&rt, &a), NodeState::Stale);
    let effects = rt.complete(&old, Outcome::Produced(value(99)), at(2));
    let new = &tickets(&effects)[0].run;
    assert_ne!(old.id(), new.id());
    assert_eq!(state(&rt, &a), NodeState::Running);
    assert!(rt.value_of(&a).is_none());
}

#[test]
fn stale_execution_requires_reactive_policy_and_repeatability_but_refresh_is_explicit() {
    for policy in [Policy::Manual, Policy::Reactive] {
        for traits in [SAFE, UNSAFE] {
            let mut rt = Runtime::new();
            rt.set_default_policy(policy);
            let a = rt.add("operation", [], traits).unwrap();
            let run = tickets(&rt.start(at(0)))[0].run.clone();
            assert!(rt.enter(&run));
            rt.complete(&run, Outcome::Produced(value(1)), at(1));
            let effects = rt.invalidate(&a, at(2)).unwrap();
            if policy == Policy::Reactive && traits.repeatable {
                assert_eq!(tickets(&effects).len(), 1);
            } else {
                assert!(tickets(&effects).is_empty());
                assert!(rt.start(at(3)).is_empty());
                assert_eq!(tickets(&rt.refresh(&a, at(4)).unwrap()).len(), 1);
            }
        }
    }
}

#[test]
fn deadline_replacement_keeps_original_start_and_rejects_old_revision() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    rt.set_timeout(&a, at(10)).unwrap();
    let effects = rt.start(at(100));
    let old = watches(&effects)[0].clone();
    let run = tickets(&effects)[0].run.clone();
    assert!(rt.enter(&run));
    let replacement = watches(&rt.set_timeout(&a, at(20)).unwrap())[0].clone();
    assert!(rt.expire(&old, at(110)).is_empty());
    assert_eq!(replacement.remaining(at(110)), at(10));
    assert!(rt.expire(&replacement, at(119)).is_empty());
    let effects = rt.expire(&replacement, at(120));
    assert_eq!(
        observations(&effects)[0].error.as_ref().unwrap().code(),
        "RUN002"
    );
    assert_eq!(state(&rt, &a), NodeState::Cancelled);
    assert!(rt.is_executing(&a));
    rt.complete(&run, Outcome::Produced(value(1)), at(121));
    let next = rt.refresh(&a, at(122)).unwrap();
    assert_eq!(tickets(&next).len(), 1);
    assert!(rt.expire(&replacement, at(1000)).is_empty());
    assert_eq!(state(&rt, &a), NodeState::Running);
}

#[test]
fn interactive_work_has_no_implicit_deadline_but_explicit_timeout_applies() {
    let mut rt = Runtime::new();
    let a = rt
        .add(
            "conversation",
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: false,
            },
        )
        .unwrap();
    let effects = rt.start(at(0));
    assert!(watches(&effects).is_empty());
    let deadline = watches(&rt.set_timeout(&a, at(5)).unwrap())[0].clone();
    assert_eq!(deadline.remaining(at(3)), at(2));
    rt.expire(&deadline, at(5));
    assert_eq!(state(&rt, &a), NodeState::Cancelled);
    assert_eq!(
        rt.set_timeout(&a, Duration::ZERO).unwrap_err(),
        RuntimeError::InvalidTimeout
    );
    assert_eq!(
        rt.set_timeout(&a, Duration::MAX).unwrap_err(),
        RuntimeError::InvalidTimeout
    );
}

#[test]
fn shortening_an_elapsed_deadline_expires_immediately() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    rt.start(at(20));
    let deadline = watches(&rt.set_timeout(&a, at(1)).unwrap())[0].clone();
    assert_eq!(deadline.remaining(at(22)), Duration::ZERO);
    rt.expire(&deadline, at(22));
    assert_eq!(state(&rt, &a), NodeState::Cancelled);
}

#[test]
fn historical_snapshots_keep_their_original_run_and_value() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let old = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&old));
    let observed = observations(&rt.complete(&old, Outcome::Produced(value(1)), at(1)))[0].clone();
    let new = tickets(&rt.refresh(&a, at(2)).unwrap())[0].run.clone();
    assert!(rt.enter(&new));
    rt.complete(&new, Outcome::Produced(value(2)), at(3));
    assert_eq!(observed.run.as_ref(), Some(old.id()));
    assert_eq!(observed.value, Some(value(1)));
    assert_eq!(observed.state, NodeState::Ready);
}

#[test]
fn restored_nodes_and_handlers_never_execute_until_explicitly_released() {
    let mut rt = Runtime::new();
    rt.set_default_policy(Policy::Reactive);
    let a = NodeId::new("legacy-root").unwrap();
    let b = NodeId::new("legacy-handler").unwrap();
    let error = RuntimeCode::ExecutionFailed.error("old failure", None);
    rt.restore(
        a.clone(),
        "producer",
        [],
        SAFE,
        RestoredState::Failed(error),
        Some(RunId::new("legacy-run").unwrap()),
    )
    .unwrap();
    rt.restore(
        b.clone(),
        "handler",
        [output(&a, OutputPort::Error)],
        SAFE,
        RestoredState::Stale,
        None,
    )
    .unwrap();
    assert!(rt.start(at(0)).is_empty());
    assert!(rt.start(at(1)).is_empty());
    assert!(rt.is_idle());
    let effects = rt.refresh(&b, at(2)).unwrap();
    assert_eq!(tickets(&effects)[0].run.node(), &b);
    assert_eq!(state(&rt, &a), NodeState::Failed);
}

#[test]
fn shutdown_does_not_schedule_cancellation_handlers_or_accept_late_results() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("cancel handler", [output(&a, OutputPort::Cancel)], SAFE)
        .unwrap();
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    let effects = rt.close();
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::Cancel(_)));
    assert_eq!(state(&rt, &b), NodeState::Pending);
    assert!(rt.start(at(1)).is_empty());
    assert!(
        rt.complete(&run, Outcome::Produced(value(1)), at(2))
            .is_empty()
    );
    assert!(rt.is_idle());
    assert_eq!(rt.add("no", [], SAFE).unwrap_err(), RuntimeError::Closed);
}

#[test]
fn drop_revokes_descendants_and_late_exit_cannot_resurrect_or_reuse_identity() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt.add("child", [OutputRef::data(a.clone())], SAFE).unwrap();
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    let (removed, effects) = rt.drop_node(&a).unwrap();
    assert!(removed.contains(&a) && removed.contains(&b));
    assert_eq!(effects.len(), 1);
    assert!(!rt.is_idle());
    assert_eq!(
        rt.restore(a.clone(), "reused", [], SAFE, RestoredState::Stale, None)
            .unwrap_err(),
        RuntimeError::Busy(a.clone())
    );
    rt.complete(&run, Outcome::Produced(value(1)), at(1));
    assert!(rt.is_idle());
    assert!(rt.graph().is_empty());
    assert_ne!(root(&mut rt), a);
}

#[test]
fn eviction_of_previous_value_does_not_make_current_executor_stale() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let old = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&old));
    rt.complete(&old, Outcome::Produced(value(1)), at(1));
    let new = tickets(&rt.refresh(&a, at(2)).unwrap())[0].run.clone();
    assert!(rt.enter(&new));
    assert!(rt.forget(&a).is_empty());
    assert_eq!(state(&rt, &a), NodeState::Running);
    rt.complete(&new, Outcome::Produced(value(2)), at(3));
    assert_eq!(state(&rt, &a), NodeState::Ready);
    assert_eq!(observations(&rt.forget(&a))[0].state, NodeState::Stale);
    assert_eq!(rt.output(&OutputRef::data(a)), OutputState::Pending);
}

#[test]
fn cancel_timeout_and_completion_permutations_have_one_winning_terminal_outcome() {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let mut rt = Runtime::new();
        let a = root(&mut rt);
        rt.set_timeout(&a, at(1)).unwrap();
        let effects = rt.start(at(0));
        let run = tickets(&effects)[0].run.clone();
        let deadline = watches(&effects)[0].clone();
        assert!(rt.enter(&run));
        let mut terminal = vec![];
        for operation in order {
            let effects = match operation {
                0 => rt.cancel(&a, at(2)),
                1 => rt.expire(&deadline, at(2)),
                _ => rt.complete(&run, Outcome::Produced(value(7)), at(2)),
            };
            terminal.extend(observations(&effects));
        }
        assert_eq!(terminal.len(), 1);
        assert!(rt.is_idle());
        assert_eq!(
            terminal[0].state,
            if order[0] == 2 {
                NodeState::Ready
            } else {
                NodeState::Cancelled
            }
        );
        if order[0] != 2 {
            assert_eq!(
                terminal[0].error.as_ref().unwrap().code(),
                if order[0] == 0 { "RUN003" } else { "RUN002" }
            );
        }
    }
}

#[test]
fn a_long_closed_branch_propagates_without_recursion_or_executor_calls() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let mut previous = a.clone();
    for _ in 0..10_000 {
        previous = rt
            .add("handler", [output(&previous, OutputPort::Cancel)], SAFE)
            .unwrap();
    }
    let run = tickets(&rt.start(at(0)))[0].run.clone();
    assert!(rt.enter(&run));
    let effects = rt.complete(&run, Outcome::Produced(value(0)), at(1));
    assert!(tickets(&effects).is_empty());
    assert_eq!(state(&rt, &previous), NodeState::Skipped);
    assert_eq!(observations(&effects).len(), 10_001);
}

#[path = "runtime/streams.rs"]
mod streams;

#[path = "runtime/downstream.rs"]
mod downstream;

#[test]
fn stale_causes_are_captured_at_invalidation_and_cleared_on_new_execution() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("dependent", [OutputRef::data(a.clone())], SAFE)
        .unwrap();
    let effects = rt.invalidate(&a, at(0)).unwrap();
    assert_eq!(rt.stale_reason(&a), Some(StaleReason::DefinitionChanged));
    assert_eq!(rt.stale_reason(&b), Some(StaleReason::DependencyChanged));
    let captured = observations(&effects);
    assert_eq!(
        captured[0].stale_reason,
        Some(StaleReason::DefinitionChanged)
    );
    assert_eq!(
        captured[1].stale_reason,
        Some(StaleReason::DependencyChanged)
    );
    let refreshed = rt.refresh(&a, at(1)).unwrap();
    assert_eq!(rt.stale_reason(&a), None);
    assert_eq!(rt.stale_reason(&b), Some(StaleReason::DependencyChanged));
    assert!(
        observations(&refreshed)
            .iter()
            .any(|o| o.node == a && o.stale_reason == Some(StaleReason::RefreshRequested))
    );
    // Captured evidence is immutable even after a later transition.
    assert_eq!(
        captured[1].stale_reason,
        Some(StaleReason::DependencyChanged)
    );
    let run = &tickets(&refreshed)[0].run;
    assert!(rt.enter(run));
    rt.complete(run, Outcome::Produced(value(1)), at(2));
    assert_eq!(rt.stale_reason(&a), None);
    let forgotten = rt.forget(&a);
    assert_eq!(
        observations(&forgotten)[0].stale_reason,
        Some(StaleReason::ResultEvicted)
    );
    assert_eq!(rt.stale_reason(&a), Some(StaleReason::ResultEvicted));
}

#[test]
fn selected_output_waits_are_run_specific_and_never_fabricate_a_success_value() {
    let mut rt = Runtime::new();
    rt.set_default_policy(Policy::Reactive);
    let a = root(&mut rt);
    let b = rt
        .add("on-error", [output(&a, OutputPort::Error)], SAFE)
        .unwrap();
    assert!(!rt.waiting_inputs(&b)[0].closed);
    let first = tickets(&rt.start(at(0)))[0].run.clone();
    rt.enter(&first);
    rt.complete(&first, Outcome::Produced(value(7)), at(1));
    let waits = rt.waiting_inputs(&b);
    assert_eq!(waits.len(), 1);
    assert!(waits[0].closed);
    assert_eq!(waits[0].run.as_ref(), Some(first.id()));
    assert!(waits[0].message().contains("error output was not produced"));
    assert!(rt.value_of(&b).is_none());
    let effects = rt.refresh(&a, at(2)).unwrap();
    let second = tickets(&effects)[0].run.clone();
    assert_eq!(state(&rt, &b), NodeState::Stale);
    assert!(rt.waiting_inputs(&b).is_empty()); // stale is not a pending selected-port wait
    rt.enter(&second);
    let next = rt.complete(
        &second,
        Outcome::Failed(RuntimeCode::ExecutionFailed.error("synthetic", None)),
        at(3),
    );
    assert!(rt.waiting_inputs(&b).is_empty());
    assert!(tickets(&next).iter().any(|t| t.run.node() == &b));
}
