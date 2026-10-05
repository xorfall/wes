use super::*;
use wes_engine::workspace::DeclarationDraft;

fn stage(draft: &mut DeclarationDraft, text: &str) {
    let Preparation::Change(change) = draft.prepare(&statement(text)).unwrap() else {
        panic!("finite declaration expected")
    };
    draft.stage(change).unwrap();
}

fn staged_control(draft: &mut DeclarationDraft, text: &str) {
    let Preparation::Meta(meta) = draft.prepare(&statement(text)).unwrap() else {
        panic!("control")
    };
    let control = draft.prepare_control(meta).unwrap();
    draft.stage_control(control).unwrap();
}

#[test]
fn recorded_change_keeps_last_actual_typing_ahead_of_its_new_prediction_during_analysis() {
    let (mut workspace, _) = workspace();
    let old = commit(&mut workspace, "catalog echo value:first > old").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&work.run);
    workspace.complete(&work.run, Outcome::Produced(int(7)), Duration::ZERO);
    let mut draft = workspace.draft().unwrap();
    staged_control(&mut draft, ":change $old value:different");
    // Echo predicts Unknown, but its last accepted result is still known to be Int.
    stage(&mut draft, "catalog count amount:$old > counted");
    assert_eq!(
        workspace.runtime().graph().node(&old).unwrap().state(),
        NodeState::Ready
    );
    let applied = workspace
        .commit_batch(draft.finish(), Duration::ZERO)
        .unwrap();
    assert_eq!(applied.changes.len(), 2);
    assert_eq!(
        workspace.runtime().graph().node(&old).unwrap().state(),
        NodeState::Stale
    );
    assert_eq!(
        workspace.data_typing(&old).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
}

#[test]
fn draft_controls_reject_stale_foreign_and_nonrecorded_preparations() {
    let (mut workspace, _) = workspace();
    commit(&mut workspace, "catalog echo value:old > old");
    let mut first = workspace.draft().unwrap();
    let mut second = workspace.draft().unwrap();
    let Preparation::Meta(meta) = first.prepare(&statement(":name unbind \"old\"")).unwrap() else {
        panic!("drop")
    };
    let control = first.prepare_control(meta).unwrap();
    assert!(matches!(
        second.stage_control(control),
        Err(WorkspaceError::Obsolete)
    ));
    let Preparation::Meta(meta) = first
        .prepare(&statement(":timeout $old after:PT1S"))
        .unwrap()
    else {
        panic!("timeout")
    };
    stage(&mut first, "$old > alias");
    assert!(matches!(
        first.prepare_control(meta),
        Err(WorkspaceError::Obsolete)
    ));
    for text in [":cancel $old", ":refresh $old"] {
        let Preparation::Meta(meta) = first.prepare(&statement(text)).unwrap() else {
            panic!("immediate")
        };
        assert!(matches!(
            first.prepare_control(meta),
            Err(WorkspaceError::Rejected { .. })
        ));
    }
    assert!(workspace.resolve("old").is_some());
}

#[test]
fn chained_declarations_share_analysis_without_installing_or_starting_live_nodes() {
    let (mut workspace, calls) = workspace();
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, ":def inner as catalog count amount:?n");
    stage(&mut draft, ":def outer as inner n:?value");
    stage(&mut draft, "outer value:4 > first *> problem");
    stage(&mut draft, "$first > alias");
    stage(&mut draft, "catalog count amount:$alias > second");
    stage(&mut draft, "catalog echo value:$problem > handler");
    assert!(
        draft
            .diagnostics()
            .all(|diagnostic| diagnostic.code != "TMP000")
    );
    assert!(workspace.runtime().graph().is_empty());
    assert!(workspace.resolve("first").is_none());
    assert!(!workspace.templates().contains("inner"));
    assert!(workspace.start(Duration::ZERO).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let batch = draft.finish();
    assert_eq!(batch.len(), 6);
    assert_eq!(
        batch.nodes().map(NodeId::as_str).collect::<Vec<_>>(),
        ["id1000", "id1001", "id1002"]
    );
    let applied = workspace
        .commit_batch(batch, std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(
        applied
            .changes
            .iter()
            .flat_map(|a| &a.diagnostics)
            .filter(|d| d.code == "TMP000")
            .count(),
        2
    );
    assert_eq!(workspace.resolve("alias"), workspace.resolve("first"));
    assert_eq!(
        workspace.resolve("problem").unwrap().port,
        OutputPort::Error
    );
    assert!(
        workspace
            .runtime()
            .graph()
            .nodes()
            .all(|node| node.state() == NodeState::Pending)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn rejected_statement_does_not_consume_names_or_ids_and_later_statements_can_succeed() {
    let (mut workspace, _) = workspace();
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, "catalog count amount:1 > id1000");
    assert_eq!(draft.resolve("id1000").unwrap().node.as_str(), "id1001");
    assert!(
        draft
            .prepare(&statement("catalog count amount:bad > rejected"))
            .is_err()
    );
    assert!(draft.resolve("rejected").is_none());
    stage(&mut draft, "catalog count amount:2 > last");
    assert_eq!(draft.resolve("last").unwrap().node.as_str(), "id1002");
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(workspace.runtime().graph().len(), 2);
    assert!(workspace.resolve("rejected").is_none());
}

#[test]
fn abandoning_a_draft_does_not_consume_live_identity_or_install_contracts() {
    let (mut workspace, _) = workspace();
    let mut draft = workspace.draft().unwrap();
    let types = draft
        .prepare_type_package("types: {Positive: {base: Int, min: 1}}", Span::at(0))
        .unwrap();
    draft.stage(types).unwrap();
    stage(
        &mut draft,
        ":def checked(n: Positive) as catalog count amount:?n",
    );
    assert!(draft.prepare(&statement("checked n:0")).is_err());
    stage(&mut draft, "checked n:3 > result");
    drop(draft);
    assert!(workspace.contracts().resolve("Positive").is_err());
    assert!(!workspace.templates().contains("checked"));
    assert_eq!(
        commit(&mut workspace, "catalog count amount:7")
            .unwrap()
            .as_str(),
        "id1000"
    );
}

#[test]
fn staged_contracts_and_captured_template_guards_commit_together() {
    let (mut workspace, _) = workspace();
    let mut draft = workspace.draft().unwrap();
    draft
        .stage(
            draft
                .prepare_type_package("types: {Positive: {base: Int, min: 1}}", Span::at(0))
                .unwrap(),
        )
        .unwrap();
    stage(
        &mut draft,
        ":def checked(n: Positive) as catalog count amount:?n",
    );
    stage(&mut draft, "checked n:3 > result");
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert!(workspace.contracts().resolve("Positive").is_ok());
    assert!(workspace.prepare(&statement("checked n:0")).is_err());
    assert!(workspace.resolve("result").is_some());
}

#[test]
fn private_stamps_reject_cross_draft_live_and_obsolete_preparations() {
    let (mut workspace, _) = workspace();
    let mut first = workspace.draft().unwrap();
    let mut second = workspace.draft().unwrap();
    let Preparation::Change(cross) = first.prepare(&statement("catalog count amount:1")).unwrap()
    else {
        panic!()
    };
    assert!(matches!(second.stage(cross), Err(WorkspaceError::Obsolete)));
    let Preparation::Change(live) = first.prepare(&statement("catalog count amount:1")).unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        workspace.commit(live),
        Err(WorkspaceError::Obsolete)
    ));
    let Preparation::Change(old) = first.prepare(&statement("catalog count amount:1")).unwrap()
    else {
        panic!()
    };
    stage(&mut first, "catalog count amount:2");
    assert!(matches!(first.stage(old), Err(WorkspaceError::Obsolete)));
    assert!(matches!(
        first.stage(prepare(&workspace, "catalog count amount:3")),
        Err(WorkspaceError::Obsolete)
    ));
    workspace
        .commit_batch(first.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert!(matches!(
        workspace.commit_batch(second.finish(), std::time::Duration::ZERO),
        Err(WorkspaceError::Obsolete)
    ));
    assert_eq!(workspace.runtime().graph().len(), 1);
}

#[test]
fn competing_structural_change_rejects_the_entire_batch() {
    let (mut workspace, _) = workspace();
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, ":def hidden as catalog count amount:4");
    stage(&mut draft, "hidden > first");
    stage(&mut draft, "catalog count amount:5 > second");
    commit(&mut workspace, "catalog count amount:9 > independent");
    assert!(matches!(
        workspace.commit_batch(draft.finish(), std::time::Duration::ZERO),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(!workspace.templates().contains("hidden"));
    assert!(workspace.resolve("first").is_none());
    assert!(workspace.resolve("second").is_none());
    assert_eq!(workspace.runtime().graph().len(), 1);
}

#[test]
fn live_completion_is_preserved_when_an_older_analysis_snapshot_commits() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog echo value:one > root").unwrap();
    let running = ticket(workspace.start(Duration::ZERO));
    assert!(workspace.enter(&running.run));
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, "catalog echo value:$root > child");
    workspace.complete(
        &running.run,
        Outcome::Produced(int(42)),
        Duration::from_secs(1),
    );
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(workspace.runtime().value_of(&root), Some(&int(42)));
    assert_eq!(
        workspace.data_typing(&root).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
    let child = ticket(workspace.start(Duration::from_secs(2)));
    assert_eq!(child.run.node(), &workspace.resolve("child").unwrap().node);
    assert_eq!(child.inputs[&root], int(42));
}

#[test]
fn cancellation_and_physical_lease_are_not_replaced_by_draft_commit() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog count amount:1 > root").unwrap();
    let running = ticket(workspace.start(Duration::ZERO));
    assert!(workspace.enter(&running.run));
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, "catalog echo value:$root::cancel > handler");
    workspace.cancel(&root, Duration::from_secs(1));
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(workspace.runtime().is_executing(&root));
    assert!(workspace.runtime().error_of(&root).is_some());
    workspace.complete(
        &running.run,
        Outcome::Produced(int(99)),
        Duration::from_secs(2),
    );
    assert!(!workspace.runtime().is_executing(&root));
    assert!(workspace.runtime().value_of(&root).is_none());
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Cancelled
    );
}

#[test]
fn meta_preparation_does_not_apply_controls_to_the_live_workspace() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog count amount:1 > root").unwrap();
    let mut draft = workspace.draft().unwrap();
    stage(&mut draft, "$root > alias");
    let Preparation::Meta(meta) = draft
        .prepare(&statement(":node remove $alias scope:downstream"))
        .unwrap()
    else {
        panic!("meta continuation expected")
    };
    assert!(matches!(
        workspace.prepare_control(meta),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(workspace.runtime().graph().node(&root).is_some());
    assert!(workspace.resolve("alias").is_none());
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert!(workspace.resolve("alias").is_some());
}
