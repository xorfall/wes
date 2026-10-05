use wes_engine::graph::{DependencyGraph, GraphError, NodeId, NodeState, OutputPort, OutputRef};

#[test]
fn admission_candidates_skip_only_exact_reserved_names_without_consuming_ids() {
    let mut graph = DependencyGraph::new();
    let candidate = graph
        .next_id_avoiding(["id1000", "id1001", "id01002", "id+1002", "later"])
        .unwrap();
    assert_eq!(candidate.as_str(), "id1002");
    assert_eq!(graph.next_id_avoiding([]).unwrap().as_str(), "id1000");
    assert!(
        graph
            .restore(
                candidate.clone(),
                "bad",
                [OutputRef::data(NodeId::new("missing").unwrap())]
            )
            .is_err()
    );
    assert_eq!(graph.next_id_avoiding([]).unwrap().as_str(), "id1000");
    graph.restore(candidate, "ok", []).unwrap();
    assert_eq!(graph.add("later", []).unwrap().as_str(), "id1003");
}

#[test]
fn reserved_names_cannot_overflow_an_exhausted_identity_sequence() {
    let mut graph = DependencyGraph::new();
    graph
        .restore(NodeId::new(format!("id{}", i64::MAX - 1)).unwrap(), (), [])
        .unwrap();
    let last = format!("id{}", i64::MAX);
    assert!(matches!(
        graph.next_id_avoiding([last.as_str()]),
        Err(GraphError::IdExhausted)
    ));
    assert_eq!(graph.add((), []).unwrap().as_str(), last);
}

#[test]
fn ids_begin_at_reference_origin_and_restore_preserves_identity() {
    let mut graph = DependencyGraph::new();
    assert_eq!(graph.add("one", []).unwrap().as_str(), "id1000");
    graph
        .restore(NodeId::new("id1007").unwrap(), "restored", [])
        .unwrap();
    assert_eq!(graph.add("next", []).unwrap().as_str(), "id1008");
    graph
        .restore(NodeId::new("legacy-name").unwrap(), "custom", [])
        .unwrap();
    assert_eq!(graph.add("last", []).unwrap().as_str(), "id1009");
    graph.clear();
    assert!(graph.is_empty());
    assert_eq!(graph.add("fresh", []).unwrap().as_str(), "id1000");
}

#[test]
fn a_diamond_invalidates_once_and_keeps_unrelated_payloads() {
    let mut graph = DependencyGraph::new();
    let root = graph.add("root", []).unwrap();
    let left = graph.add("left", [OutputRef::data(root.clone())]).unwrap();
    let right = graph.add("right", [OutputRef::data(root.clone())]).unwrap();
    let both = graph
        .add(
            "both",
            [
                OutputRef::data(left.clone()),
                OutputRef::data(right.clone()),
            ],
        )
        .unwrap();
    let other = graph.add("other", []).unwrap();
    graph.set_state(&root, NodeState::Ready).unwrap();
    assert_eq!(
        graph
            .mark_stale(&root)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [root.clone(), left.clone(), right.clone(), both.clone()]
    );
    assert_eq!(*graph.node(&root).unwrap().payload(), "root");
    assert_eq!(graph.node(&other).unwrap().state(), NodeState::Pending);
    let order = graph.execution_order();
    for node in graph.nodes() {
        for dependency in node.dependencies().keys() {
            assert!(
                order.iter().position(|id| id == dependency)
                    < order.iter().position(|id| id == node.id())
            );
        }
    }
    graph.remove(&left).unwrap();
    assert!(graph.node(&both).is_none());
    assert!(graph.node(&right).is_some());
    assert_eq!(
        graph.dependents_of(&root).cloned().collect::<Vec<_>>(),
        [right]
    );
}

#[test]
fn rejected_insertions_do_not_consume_ids_or_mutate_reverse_edges() {
    let mut graph = DependencyGraph::new();
    let root = graph.add(1, []).unwrap();
    assert!(
        graph
            .add(2, [OutputRef::data(NodeId::new("missing").unwrap())])
            .is_err()
    );
    assert!(graph.restore(root.clone(), 9, []).is_err());
    assert!(
        graph
            .add(
                3,
                [
                    OutputRef::data(root.clone()),
                    OutputRef {
                        node: root.clone(),
                        port: OutputPort::Error
                    }
                ]
            )
            .is_err()
    );
    assert_eq!(graph.dependents_of(&root).count(), 0);
    assert_eq!(graph.len(), 1);
    assert_eq!(graph.add(4, []).unwrap().as_str(), "id1001");
}

#[test]
fn duplicate_same_output_dependencies_are_one_edge() {
    let mut graph = DependencyGraph::new();
    let root = graph.add(1, []).unwrap();
    let child = graph
        .add(
            2,
            [OutputRef::data(root.clone()), OutputRef::data(root.clone())],
        )
        .unwrap();
    assert_eq!(graph.node(&child).unwrap().dependencies().len(), 1);
    assert_eq!(graph.execution_order(), [root, child]);
}

#[test]
fn payload_changes_never_repoint_dependencies() {
    let mut graph = DependencyGraph::new();
    let root = graph.add(1, []).unwrap();
    let child = graph
        .add(
            2,
            [OutputRef {
                node: root.clone(),
                port: OutputPort::Cancel,
            }],
        )
        .unwrap();
    assert_eq!(graph.replace_payload(&child, 3).unwrap(), 2);
    assert_eq!(
        graph.node(&child).unwrap().dependencies()[&root],
        OutputPort::Cancel
    );
    assert_eq!(graph.node(&child).unwrap().state(), NodeState::Pending);
}

#[test]
fn exhausting_generated_identity_space_never_wraps_or_reuses_an_id() {
    let mut graph = DependencyGraph::new();
    graph
        .restore(NodeId::new("id9223372036854775806").unwrap(), 1, [])
        .unwrap();
    assert_eq!(graph.add(2, []).unwrap().as_str(), "id9223372036854775807");
    assert_eq!(graph.add(3, []).unwrap_err(), GraphError::IdExhausted);
    assert_eq!(graph.len(), 2);
}

#[test]
fn long_chains_are_traversed_iteratively_and_removed_without_recursion() {
    let mut graph = DependencyGraph::new();
    let root = graph.add(0, []).unwrap();
    let mut previous = root.clone();
    for i in 1..10_000 {
        previous = graph.add(i, [OutputRef::data(previous)]).unwrap();
    }
    assert_eq!(graph.execution_order().len(), 10_000);
    assert_eq!(graph.remove(&root).unwrap().len(), 10_000);
    assert!(graph.is_empty());
}
