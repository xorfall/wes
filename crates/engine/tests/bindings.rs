use wes_engine::{
    bindings::{BindingError, Bindings},
    graph::{DependencyGraph, OutputPort, OutputRef},
};

#[test]
fn aliases_and_explicit_selectors_resolve_the_same_node_with_different_channels() {
    let mut graph = DependencyGraph::new();
    let node = graph.add(1, []).unwrap();
    let mut names = Bindings::new();
    names
        .bind("result", OutputRef::data(node.clone()), &graph)
        .unwrap();
    names
        .bind(
            "failure",
            OutputRef {
                node: node.clone(),
                port: OutputPort::Error,
            },
            &graph,
        )
        .unwrap();
    assert_eq!(
        names.resolve("result::error", &graph),
        names.resolve("failure", &graph)
    );
    assert_eq!(
        names.resolve("failure::data", &graph),
        names.resolve("result", &graph)
    );
    assert_eq!(
        names
            .resolve(&format!("{node}::cancel"), &graph)
            .unwrap()
            .port,
        OutputPort::Cancel
    );
    assert!(names.resolve("result::unknown", &graph).is_none());
    assert!(names.resolve("result::error::data", &graph).is_none());
    assert_eq!(
        names.names_of(&node).collect::<Vec<_>>(),
        ["result", "failure"]
    );
}

#[test]
fn rebinding_a_name_does_not_rewrite_a_previously_resolved_edge() {
    let mut graph = DependencyGraph::new();
    let first = graph.add(1, []).unwrap();
    let second = graph.add(2, []).unwrap();
    let mut names = Bindings::new();
    names
        .bind("current", OutputRef::data(first.clone()), &graph)
        .unwrap();
    let captured = names.resolve("current", &graph).unwrap();
    names
        .bind("current", OutputRef::data(second.clone()), &graph)
        .unwrap();
    assert_eq!(captured.node, first);
    assert_eq!(names.resolve("current", &graph).unwrap().node, second);
}

#[test]
fn invalid_or_shadowing_names_do_not_overwrite_bindings() {
    let mut graph = DependencyGraph::new();
    let node = graph.add(1, []).unwrap();
    let mut names = Bindings::new();
    assert!(matches!(
        names.bind(node.to_string(), OutputRef::data(node.clone()), &graph),
        Err(BindingError::ShadowsId(_))
    ));
    for name in ["", "two words", "x::error", "error-rate", "Ⅷ", "😀"] {
        assert!(
            names
                .bind(name, OutputRef::data(node.clone()), &graph)
                .is_err()
        );
    }
    assert!(Bindings::validate_names(["same", "same"], &graph).is_err());
    assert!(names.names().is_empty());
    names.bind("ürün_1", OutputRef::data(node), &graph).unwrap();
    assert_eq!(names.names().len(), 1);
}

#[test]
fn dropping_nodes_cannot_leave_resolvable_dangling_aliases() {
    let mut graph = DependencyGraph::new();
    let node = graph.add(1, []).unwrap();
    let mut names = Bindings::new();
    names
        .bind("result", OutputRef::data(node.clone()), &graph)
        .unwrap();
    graph.remove(&node).unwrap();
    assert!(names.resolve("result", &graph).is_none());
    names.retain_nodes(&graph);
    assert!(names.names().is_empty());
}

#[test]
fn observed_tables_use_the_live_binding_resolution_rules_without_copying() {
    let mut graph = DependencyGraph::new();
    let node = graph.add(1, []).unwrap();
    let mut bindings = Bindings::new();
    bindings
        .bind("result", OutputRef::data(node.clone()), &graph)
        .unwrap();
    for written in [
        "result".to_string(),
        node.to_string(),
        format!("{node}::error"),
    ] {
        assert_eq!(
            Bindings::resolve_names(bindings.names(), &written, &graph),
            bindings.resolve(&written, &graph)
        );
    }
    assert!(Bindings::resolve_names(bindings.names(), "id999999999", &graph).is_none());
}
