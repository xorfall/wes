use super::*;
use std::collections::HashSet;

fn seeded(
    rt: &mut Runtime<&'static str>,
    name: &'static str,
    dependencies: Vec<OutputRef>,
    traits: ExecutionTraits,
    policy: Policy,
) -> NodeId {
    let id = rt.graph().next_id_avoiding([]).unwrap();
    rt.restore(
        id.clone(),
        name,
        dependencies,
        traits,
        RestoredState::Ready(value(1)),
        Some(RunId::new(format!("old-{name}")).unwrap()),
    )
    .unwrap();
    rt.set_policy(&id, policy).unwrap();
    id
}
fn configs() -> [(ExecutionTraits, Policy); 4] {
    [
        (SAFE, Policy::Manual),
        (SAFE, Policy::Reactive),
        (UNSAFE, Policy::Manual),
        (UNSAFE, Policy::Reactive),
    ]
}
fn finish(
    rt: &mut Runtime<&'static str>,
    ticket: &RunTicket<&'static str>,
    number: i64,
) -> Vec<Effect<&'static str>> {
    assert!(rt.enter(&ticket.run));
    rt.complete(&ticket.run, Outcome::Produced(value(number)), at(3))
}
fn snapshot(
    rt: &Runtime<&'static str>,
    nodes: &[NodeId],
) -> Vec<(NodeState, Option<Value>, Option<RunId>)> {
    nodes
        .iter()
        .map(|node| {
            (
                state(rt, node),
                rt.value_of(node).cloned(),
                rt.run_of(node).cloned(),
            )
        })
        .collect()
}

#[test]
fn chain_refresh_is_explicit_for_every_policy_trait_combination_and_equal_result() {
    for (root_traits, root_policy) in configs() {
        for (middle_traits, middle_policy) in configs() {
            for (leaf_traits, leaf_policy) in configs() {
                let mut rt = Runtime::new();
                let a = seeded(&mut rt, "a", vec![], root_traits, root_policy);
                let b = seeded(
                    &mut rt,
                    "b",
                    vec![OutputRef::data(a.clone())],
                    middle_traits,
                    middle_policy,
                );
                let c = seeded(
                    &mut rt,
                    "c",
                    vec![OutputRef::data(b.clone())],
                    leaf_traits,
                    leaf_policy,
                );
                assert!(rt.start(at(0)).is_empty()); // restored state never starts by itself
                let selected = [a.clone(), b.clone(), c.clone()];
                let old = snapshot(&rt, &selected);
                let effects = rt.refresh_downstream(&a, at(1)).unwrap();
                let stale = observations(&effects)
                    .into_iter()
                    .filter(|o| o.state == NodeState::Stale)
                    .map(|o| o.node)
                    .collect::<Vec<_>>();
                assert_eq!(stale, selected); // one invalidation per member
                let first = tickets(&effects);
                assert_eq!(first.len(), 1);
                let middle = tickets(&finish(&mut rt, &first[0], 1));
                assert_eq!(middle.len(), 1);
                assert_eq!(middle[0].run.node(), &b);
                assert_eq!(middle[0].inputs[&a], value(1));
                let last = tickets(&finish(&mut rt, &middle[0], 1));
                assert_eq!(last.len(), 1);
                assert_eq!(last[0].run.node(), &c);
                assert!(tickets(&finish(&mut rt, &last[0], 1)).is_empty());
                for (index, node) in selected.iter().enumerate() {
                    assert_eq!(state(&rt, node), NodeState::Ready);
                    assert_ne!(rt.run_of(node), old[index].2.as_ref());
                }
                assert!(rt.start(at(4)).is_empty());
                // Ordinary refresh remains policy/trait governed after one-shot intent is spent.
                let first = tickets(&rt.refresh(&a, at(5)).unwrap());
                let ordinary = tickets(&finish(&mut rt, &first[0], 1));
                assert_eq!(
                    ordinary.len(),
                    usize::from(middle_policy == Policy::Reactive && middle_traits.repeatable)
                );
            }
        }
    }
}

#[test]
fn diamond_never_joins_old_and_new_outputs_and_preserves_external_inputs() {
    for (left_traits, left_policy) in configs() {
        for (right_traits, right_policy) in configs() {
            for (join_traits, join_policy) in configs() {
                let mut rt = Runtime::new();
                let external = seeded(&mut rt, "external", vec![], UNSAFE, Policy::Manual);
                let a = seeded(&mut rt, "a", vec![], UNSAFE, Policy::Manual);
                let b = seeded(
                    &mut rt,
                    "b",
                    vec![OutputRef::data(a.clone())],
                    left_traits,
                    left_policy,
                );
                let c = seeded(
                    &mut rt,
                    "c",
                    vec![OutputRef::data(a.clone())],
                    right_traits,
                    right_policy,
                );
                let d = seeded(
                    &mut rt,
                    "d",
                    vec![
                        OutputRef::data(b.clone()),
                        OutputRef::data(c.clone()),
                        OutputRef::data(external.clone()),
                    ],
                    join_traits,
                    join_policy,
                );
                let outside = snapshot(&rt, &[external.clone()]);
                let effects = rt.refresh_downstream(&a, at(1)).unwrap();
                let stale = observations(&effects)
                    .into_iter()
                    .filter(|o| o.state == NodeState::Stale)
                    .map(|o| o.node)
                    .collect::<Vec<_>>();
                assert_eq!(stale.len(), 4);
                assert_eq!(stale.iter().collect::<HashSet<_>>().len(), 4);
                let first = tickets(&effects).remove(0);
                let branches = tickets(&finish(&mut rt, &first, 2));
                assert_eq!(branches.len(), 2);
                assert!(tickets(&finish(&mut rt, &branches[1], 20)).is_empty());
                assert_eq!(state(&rt, &d), NodeState::Stale);
                let join = tickets(&finish(&mut rt, &branches[0], 10));
                assert_eq!(join.len(), 1);
                assert_eq!(join[0].run.node(), &d);
                assert_eq!(join[0].inputs[&b], value(10));
                assert_eq!(join[0].inputs[&c], value(20));
                assert_eq!(join[0].inputs[&external], value(1));
                assert!(tickets(&finish(&mut rt, &join[0], 30)).is_empty());
                assert_eq!(snapshot(&rt, &[external]), outside);
            }
        }
    }
}

#[test]
fn terminal_ports_close_unselected_branches_and_consume_waiting_intent() {
    for outcome in 0..3 {
        let mut rt = Runtime::new();
        let a = seeded(&mut rt, "root", vec![], UNSAFE, Policy::Manual);
        let data = seeded(
            &mut rt,
            "data",
            vec![output(&a, OutputPort::Data)],
            UNSAFE,
            Policy::Manual,
        );
        let error = seeded(
            &mut rt,
            "error",
            vec![output(&a, OutputPort::Error)],
            UNSAFE,
            Policy::Manual,
        );
        let cancel = seeded(
            &mut rt,
            "cancel",
            vec![output(&a, OutputPort::Cancel)],
            UNSAFE,
            Policy::Manual,
        );
        let root = tickets(&rt.refresh_downstream(&a, at(0)).unwrap()).remove(0);
        assert!(rt.enter(&root.run));
        let outcome_value = match outcome {
            0 => Outcome::Produced(value(5)),
            1 => Outcome::Failed(RuntimeCode::ExecutionFailed.error("synthetic", None)),
            _ => Outcome::Cancelled(RuntimeCode::Cancelled.error("synthetic", None)),
        };
        let selected = tickets(&rt.complete(&root.run, outcome_value, at(1)));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].run.node(), [&data, &error, &cancel][outcome]);
        assert!(tickets(&finish(&mut rt, &selected[0], 6)).is_empty());
        assert!(rt.start(at(2)).is_empty());
        for node in [&data, &error, &cancel] {
            if node != selected[0].run.node() {
                assert!(matches!(
                    state(&rt, node),
                    NodeState::Skipped | NodeState::Failed
                ));
            }
        }
        // Fresh plain refresh must not replay the old explicit request in another branch.
        let next = tickets(&rt.refresh(&a, at(3)).unwrap()).remove(0);
        assert!(tickets(&finish(&mut rt, &next, 5)).is_empty());
    }
}

#[test]
fn busy_and_unbounded_descendants_reject_without_changing_the_root_or_peers() {
    for entered in [false, true] {
        let mut rt = Runtime::new();
        let a = seeded(&mut rt, "a", vec![], SAFE, Policy::Manual);
        let b = seeded(
            &mut rt,
            "b",
            vec![OutputRef::data(a.clone())],
            UNSAFE,
            Policy::Manual,
        );
        let c = seeded(
            &mut rt,
            "c",
            vec![OutputRef::data(a.clone())],
            SAFE,
            Policy::Manual,
        );
        let busy = tickets(&rt.refresh(&b, at(0)).unwrap()).remove(0);
        if entered {
            assert!(rt.enter(&busy.run));
        }
        let nodes = [a.clone(), b.clone(), c];
        let before = snapshot(&rt, &nodes);
        assert_eq!(
            rt.refresh_downstream(&a, at(1)).unwrap_err(),
            RuntimeError::Busy(b)
        );
        assert_eq!(snapshot(&rt, &nodes), before);
    }
    let mut rt = Runtime::new();
    let a = seeded(&mut rt, "a", vec![], SAFE, Policy::Manual);
    let b = seeded(
        &mut rt,
        "unbounded",
        vec![OutputRef::data(a.clone())],
        ExecutionTraits {
            pure: false,
            repeatable: false,
            bounded: false,
        },
        Policy::Manual,
    );
    let nodes = [a.clone(), b.clone()];
    let before = snapshot(&rt, &nodes);
    assert_eq!(
        rt.refresh_downstream(&a, at(1)).unwrap_err(),
        RuntimeError::UnsupportedDownstream(b)
    );
    assert_eq!(snapshot(&rt, &nodes), before);
}

#[test]
fn unavailable_external_selected_ports_reject_before_any_invalidation() {
    for port in [OutputPort::Data, OutputPort::Error, OutputPort::Cancel] {
        let mut rt = Runtime::new();
        let external = seeded(&mut rt, "outside", vec![], SAFE, Policy::Manual);
        if port == OutputPort::Data {
            rt.forget(&external);
        } // Pending; other ports Closed
        let a = seeded(&mut rt, "a", vec![], SAFE, Policy::Manual);
        let input = output(&external, port);
        let join = seeded(
            &mut rt,
            "join",
            vec![OutputRef::data(a.clone()), input.clone()],
            UNSAFE,
            Policy::Manual,
        );
        let nodes = [external, a.clone(), join.clone()];
        let before = snapshot(&rt, &nodes);
        assert_eq!(
            rt.refresh_downstream(&a, at(1)).unwrap_err(),
            RuntimeError::UnavailableDownstreamInput { node: join, input }
        );
        assert_eq!(snapshot(&rt, &nodes), before);
    }
}

#[test]
fn pending_explicit_cancellation_stays_terminal_even_for_safe_reactive_nodes() {
    for (traits, policy) in configs() {
        let mut rt = Runtime::new();
        let a = seeded(&mut rt, "a", vec![], SAFE, Policy::Manual);
        let b = seeded(
            &mut rt,
            "b",
            vec![OutputRef::data(a.clone())],
            traits,
            policy,
        );
        let c = seeded(
            &mut rt,
            "c",
            vec![OutputRef::data(b.clone())],
            UNSAFE,
            Policy::Manual,
        );
        let first = tickets(&rt.refresh_downstream(&a, at(0)).unwrap()).remove(0);
        let effects = rt.cancel(&b, at(1));
        assert!(tickets(&effects).is_empty());
        assert_eq!(state(&rt, &b), NodeState::Cancelled);
        assert_eq!(state(&rt, &c), NodeState::Skipped);
        assert!(tickets(&finish(&mut rt, &first, 2)).is_empty());
        assert!(rt.start(at(4)).is_empty());
    }
}

#[test]
fn stale_callbacks_cannot_publish_and_restoration_does_not_replay_waiting_intent() {
    let mut rt = Runtime::new();
    let a = root(&mut rt);
    let b = rt
        .add("child", [OutputRef::data(a.clone())], UNSAFE)
        .unwrap();
    let old_root = tickets(&rt.start(at(0))).remove(0);
    let old_child = tickets(&finish(&mut rt, &old_root, 1)).remove(0);
    finish(&mut rt, &old_child, 1);
    let new_root = tickets(&rt.refresh_downstream(&a, at(1)).unwrap()).remove(0);
    assert!(
        rt.complete(&old_root.run, Outcome::Produced(value(99)), at(2))
            .is_empty()
    );
    assert!(
        rt.complete(&old_child.run, Outcome::Produced(value(99)), at(2))
            .is_empty()
    );
    assert_eq!(rt.output(&OutputRef::data(b.clone())), OutputState::Pending);
    let new_child = tickets(&finish(&mut rt, &new_root, 2)).remove(0);
    assert_eq!(new_child.inputs[&a], value(2));
    assert_ne!(new_child.run.id(), old_child.run.id());
    let mut restored = Runtime::new();
    restored
        .restore(
            a.clone(),
            "root",
            [],
            SAFE,
            RestoredState::Ready(value(2)),
            rt.run_of(&a).cloned(),
        )
        .unwrap();
    restored
        .restore(
            b.clone(),
            "child",
            [OutputRef::data(a.clone())],
            UNSAFE,
            RestoredState::Stale,
            rt.run_of(&b).cloned(),
        )
        .unwrap();
    assert!(restored.start(at(3)).is_empty());
    assert_eq!(state(&restored, &b), NodeState::Stale);
    assert_eq!(
        tickets(&restored.refresh_downstream(&a, at(4)).unwrap()).len(),
        1
    );
}

#[test]
fn later_restored_members_do_not_inherit_the_admitted_request() {
    let mut rt = Runtime::new();
    let a = seeded(&mut rt, "a", vec![], UNSAFE, Policy::Manual);
    let root = tickets(&rt.refresh_downstream(&a, at(0)).unwrap()).remove(0);
    let later = rt.graph().next_id_avoiding([]).unwrap();
    rt.restore(
        later.clone(),
        "later",
        [OutputRef::data(a.clone())],
        UNSAFE,
        RestoredState::Stale,
        None,
    )
    .unwrap();
    assert!(tickets(&finish(&mut rt, &root, 2)).is_empty());
    assert_eq!(state(&rt, &later), NodeState::Stale);
}
