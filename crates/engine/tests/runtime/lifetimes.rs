use super::*;

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
