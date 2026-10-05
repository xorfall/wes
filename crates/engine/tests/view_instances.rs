use std::sync::Arc;
use wes_core::{Data, Provenance, Value};
use wes_engine::{
    graph::NodeId,
    views::{Error, Input, Limits, Store},
};
fn id(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}
fn create(store: &mut Store, name: &str, kind: &str) -> wes_engine::views::Handle {
    let definition = store.catalogue().get(kind).unwrap().clone();
    store
        .create(id(name), kind, &definition.digest, None)
        .unwrap()
}
fn card(n: Data) -> Value {
    Value::new(
        wes_views::named("Metric").unwrap().input().shape(),
        Data::Record(
            [
                ("view".into(), Data::Text("metric".into())),
                ("value".into(), n),
            ]
            .into_iter()
            .collect(),
        ),
        Provenance::default(),
    )
    .unwrap()
}
#[test]
fn edits_preserve_identity_and_share_payload_with_atomic_rejections() {
    let mut store = Store::default();
    let handle = create(&mut store, "card", "Metric");
    let input = card(Data::Int(1250));
    assert_eq!(
        store.bind(&handle, 0, Some(Input::constant(input.clone()))),
        Ok(1)
    );
    let snapshot = store.read(&handle).unwrap();
    assert_eq!(snapshot.id, id("card"));
    assert!(std::ptr::eq(
        snapshot.input.unwrap().value().unwrap().data(),
        input.data()
    ));
    assert_eq!(store.bind(&handle, 0, None), Err(Error::Revision));
    let rejected = store
        .bind(
            &handle,
            1,
            Some(Input::constant(card(Data::Text("bad".into())))),
        )
        .unwrap_err();
    let Error::InputContract { contract, issues } = rejected else {
        panic!("expected a field contract error")
    };
    assert_eq!(contract, "Metric");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "/value");
    assert_eq!(store.read(&handle).unwrap().revision, 1);
    assert_eq!(
        store.bind(&handle, 1, Some(Input::constant(card(Data::Int(2500))))),
        Ok(2)
    );
    assert_eq!(store.read(&handle).unwrap().id, id("card"));
}

#[test]
fn rejected_creation_preserves_capacity_and_distinguishes_unknown_inputs() {
    let mut store = Store::new(
        wes_views::catalogue().iter().cloned().map(Arc::new),
        Limits {
            instances: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let package = wes_views::named("Metric").unwrap();
    let error = store
        .create(
            id("card"),
            "Metric",
            &package.digest,
            Some(Input::constant(card(Data::Text("bad".into())))),
        )
        .unwrap_err();
    assert!(error.to_string().contains("/value"));
    assert!(!error.to_string().contains("Unknown"));
    let unknown = card(Data::Int(1))
        .with_shape(wes_core::Shape::Unknown)
        .unwrap();
    assert_eq!(
        store
            .create(
                id("card"),
                "Metric",
                &package.digest,
                Some(Input::constant(unknown)),
            )
            .unwrap_err(),
        Error::InputUnknown
    );
    let handle = create(&mut store, "card", "Metric");
    assert_eq!(store.read(&handle).unwrap().revision, 0);
}
#[test]
fn references_cannot_cross_owners_or_survive_deletion_and_identity_reuse() {
    let mut a = Store::default();
    let mut b = Store::default();
    let old = create(&mut a, "card", "Metric");
    create(&mut b, "card", "Metric");
    assert!(matches!(b.read(&old), Err(Error::Reference)));
    a.remove(&old, 0).unwrap();
    let new = create(&mut a, "card", "Metric");
    assert!(matches!(a.read(&old), Err(Error::Reference)));
    assert!(a.read(&new).is_ok());
}
#[test]
fn default_slots_cycles_and_coordinators_are_checked_without_component_branches() {
    let mut store = Store::default();
    let a = create(&mut store, "a", "Dashboard");
    let b = create(&mut store, "b", "Dashboard");
    let chart = create(&mut store, "chart", "Timeline");
    let first = create(&mut store, "first", "TimelineGroup");
    let second = create(&mut store, "second", "TimelineGroup");
    let card = create(&mut store, "card", "Metric");
    assert_eq!(store.connect(&b, &a, None, 0), Ok(1));
    assert_eq!(store.connect(&a, &b, None, 0), Err(Error::Cycle));
    assert_eq!(store.read(&b).unwrap().revision, 0);
    assert_eq!(store.connect(&chart, &first, None, 0), Ok(1));
    assert_eq!(store.connect(&chart, &first, None, 1), Ok(1));
    assert_eq!(
        store.connect(&chart, &second, None, 0),
        Err(Error::Coordinator)
    );
    assert_eq!(
        store.connect(&card, &second, None, 0),
        Err(Error::Incompatible)
    );
    assert_eq!(
        store.connect(&card, &a, Some("missing"), 1),
        Err(Error::Slot)
    );
    assert_eq!(store.disconnect(&chart, &first, None, 1), Ok(2));
    assert_eq!(store.connect(&chart, &second, None, 0), Ok(1));
    store.remove(&chart, 2).unwrap();
    assert!(store.read(&second).unwrap().members["members"].is_empty());
    assert_eq!(store.read(&second).unwrap().revision, 2);
}
#[test]
fn limits_and_digest_mismatch_do_not_spend_capacity_on_failure() {
    let limits = Limits {
        instances: 2,
        edges: 1,
        input_bytes: 64,
        total_input_bytes: 64,
    };
    let mut store =
        Store::new(wes_views::catalogue().iter().cloned().map(Arc::new), limits).unwrap();
    assert!(matches!(
        store.create(id("bad"), "Metric", "wrong", None),
        Err(Error::Digest)
    ));
    let definition = wes_views::named("Metric").unwrap();
    assert!(matches!(
        store.create(
            id("large"),
            "Metric",
            &definition.digest,
            Some(Input::constant(card(Data::Int(1))))
        ),
        Err(Error::Capacity)
    ));
    let a = create(&mut store, "a", "Dashboard");
    let b = create(&mut store, "b", "Dashboard");
    assert_eq!(store.connect(&b, &a, None, 0), Ok(1));
    assert!(matches!(
        store.create(id("third"), "Metric", &definition.digest, None),
        Err(Error::Capacity)
    ));
    store.remove(&b, 0).unwrap();
    let c = create(&mut store, "c", "Metric");
    assert_eq!(store.connect(&c, &a, None, 2), Ok(3));
}

#[test]
fn committed_state_has_one_coordinator_and_atomic_typed_revisions() {
    use wes_engine::views::InteractionEdit;
    let mut store = Store::default();
    let child = create(&mut store, "line", "Timeline");
    let parent = create(&mut store, "group", "TimelineGroup");
    let range = Data::Record(
        [
            (
                "start".into(),
                Data::Instant("2026-01-01T00:00:00Z".parse().unwrap()),
            ),
            (
                "end".into(),
                Data::Instant("2026-01-01T00:10:00Z".parse().unwrap()),
            ),
        ]
        .into(),
    );
    let edit = InteractionEdit {
        events: vec![],
        owner: child.id().clone(),
        identity: store.read(&child).unwrap().identity.to_string(),
        definition_revision: 0,
        revision: 0,
        fields: [
            ("viewport".into(), range),
            ("selection".into(), Data::Option(None)),
            ("selectedItem".into(), Data::Option(None)),
        ]
        .into(),
        outputs: [
            ("selection".into(), Data::Option(None)),
            ("selectedItem".into(), Data::Option(None)),
        ]
        .into(),
    };
    store.commit_interaction(&child, edit.clone()).unwrap();
    assert_eq!(
        store.connect(&child, &parent, None, 1),
        Err(Error::Revision)
    );
    assert_eq!(store.interaction(&child).unwrap().1.revision, 1);
    store.connect(&child, &parent, None, 0).unwrap();
    let (owner, empty) = store.interaction(&child).unwrap();
    assert_eq!(owner.id, id("group"));
    assert_eq!(empty.revision, 0);
    assert_eq!(store.read(&child).unwrap().revision, 1);
    let standalone = edit.clone();
    let edit = InteractionEdit {
        owner: owner.id.clone(),
        identity: owner.identity.to_string(),
        definition_revision: 1,
        ..edit
    };
    let state = store.commit_interaction(&child, edit.clone()).unwrap();
    assert_eq!(state.revision, 1);
    assert_eq!(store.interaction(&parent).unwrap().1.fields, state.fields);
    assert!(matches!(
        store.commit_interaction(&child, edit.clone()),
        Err(Error::Revision)
    ));
    let mut bad = edit.clone();
    bad.revision = 1;
    bad.fields.insert("cursor".into(), Data::Option(None));
    assert!(matches!(
        store.commit_interaction(&parent, bad),
        Err(Error::Interaction)
    ));
    assert_eq!(store.interaction(&child).unwrap().1.revision, 1);
    let mut bad = edit.clone();
    bad.revision = 1;
    bad.outputs
        .insert("selection".into(), Data::Text("invalid".into()));
    assert!(matches!(
        store.commit_interaction(&parent, bad),
        Err(Error::Interaction)
    ));
    let output = store.output(&child, "selection").unwrap();
    assert_eq!(
        output.shape(),
        &wes_core::Shape::Option(Box::new(wes_core::Shape::Primitive(
            wes_core::Primitive::Interval
        )))
    );
    assert_eq!(
        output.provenance().fact("view.instance"),
        Some(owner.identity.as_ref())
    );
    assert!(store.output(&child, "cursor").is_err());
    store.disconnect(&child, &parent, None, 1).unwrap();
    assert!(matches!(
        store.commit_interaction(&child, edit),
        Err(Error::Reference)
    ));
    assert_eq!(store.interaction(&child).unwrap().1.revision, 0);
    assert_eq!(
        store.commit_interaction(&child, standalone).unwrap_err(),
        Error::Revision
    );
}

#[test]
fn separate_coordinators_keep_committed_selections_independent() {
    use wes_engine::views::InteractionEdit;
    let mut store = Store::default();
    let first = create(&mut store, "first", "TimelineGroup");
    let second = create(&mut store, "second", "TimelineGroup");
    let a = create(&mut store, "a", "Timeline");
    let b = create(&mut store, "b", "Timeline");
    store.connect(&a, &first, None, 0).unwrap();
    store.connect(&b, &second, None, 0).unwrap();
    let interval: wes_core::Interval = "2031-01-01T00:00:00Z/2031-01-01T00:01:00Z".parse().unwrap();
    let range = Data::Record(
        [
            ("start".into(), Data::Instant(interval.start())),
            ("end".into(), Data::Instant(interval.end())),
        ]
        .into(),
    );
    let selected = Data::Option(Some(Box::new(Data::Interval(interval))));
    store
        .commit_interaction(
            &a,
            InteractionEdit {
                events: vec![],
                owner: first.id().clone(),
                identity: store.read(&first).unwrap().identity.to_string(),
                definition_revision: 1,
                revision: 0,
                fields: [
                    ("viewport".into(), range.clone()),
                    ("selection".into(), Data::Option(Some(Box::new(range)))),
                    ("selectedItem".into(), Data::Option(None)),
                ]
                .into(),
                outputs: [
                    ("selection".into(), selected.clone()),
                    ("selectedItem".into(), Data::Option(None)),
                ]
                .into(),
            },
        )
        .unwrap();
    assert_eq!(store.output(&a, "selection").unwrap().data(), &selected);
    assert_eq!(store.interaction(&b).unwrap().1.revision, 0);
    assert!(store.interaction(&b).unwrap().1.outputs.is_empty());
}

#[test]
fn output_links_validate_types_writers_and_cycles() {
    let example = wes_views::Package::parse(
        include_str!("../../../examples/view-packages/source/range-summary/view.json"),
        include_str!("../../../examples/view-packages/source/range-summary/types.yaml"),
    )
    .unwrap();
    let mut store = Store::new(
        wes_views::catalogue()
            .iter()
            .cloned()
            .chain([example])
            .map(Arc::new),
        Limits::default(),
    )
    .unwrap();
    let source = create(&mut store, "chart", "Timeline");
    let target = create(&mut store, "summary", "RangeSummary");
    let card = create(&mut store, "card", "Metric");
    assert_eq!(
        store.link(&source, "selection", &card, "value", 0),
        Err(Error::Incompatible)
    );
    assert_eq!(
        store.link(&source, "cursor", &target, "selection", 0),
        Err(Error::Interaction)
    );
    assert_eq!(
        store.link(&source, "selection", &target, "selection", 0),
        Ok(1)
    );
    assert_eq!(
        store.read(&target).unwrap().linked_inputs,
        vec!["selection"]
    );
    assert_eq!(
        store.link(&source, "selection", &target, "selection", 1),
        Err(Error::Duplicate)
    );
    assert_eq!(store.unlink(&target, "selection", 0), Err(Error::Revision));
    assert_eq!(store.unlink(&target, "selection", 1), Ok(2));
    assert!(store.read(&target).unwrap().linked_inputs.is_empty());
    // A generic package proves that cycle checks are not Timeline-specific.
    let manifest = r#"{"name":"Selector","id":"selector","summary":"A typed selection","renderer":"View.tsx","input":"Input","outputs":{"selection":{"type":"Int","mode":"state","shared":true}},"interaction":{"protocol":"Selection","state":"Input","event":"Input","sharedFields":["selection"]}}"#;
    let types = "types: {Input: {base: Record, fields: {selection: Int}}}";
    let package = Arc::new(wes_views::Package::parse(manifest, types).unwrap());
    let mut other = Store::new([package.clone()], Limits::default()).unwrap();
    let a = other
        .create(id("a"), "Selector", &package.digest, None)
        .unwrap();
    let b = other
        .create(id("b"), "Selector", &package.digest, None)
        .unwrap();
    other.link(&a, "selection", &b, "selection", 0).unwrap();
    assert_eq!(
        other.link(&b, "selection", &a, "selection", 0),
        Err(Error::Cycle)
    );
    assert_eq!(
        other.link(&a, "selection", &a, "selection", 0),
        Err(Error::Cycle)
    );
}

#[test]
fn event_windows_preserve_duplicates_reject_partial_commits_and_report_eviction() {
    use wes_engine::views::{EventEmission, InteractionEdit};
    let source=Arc::new(wes_views::Package::parse(r#"{"name":"Selector","id":"selector","summary":"Event selection","renderer":"View.tsx","input":"Input","outputs":{"picked":{"type":"Int","mode":"event","shared":true}},"interaction":{"protocol":"Selection","state":"Input","event":"Input","sharedFields":["selection"]}}"#,"types: {Input: {base: Record, fields: {selection: Int}}}").unwrap());
    let target=Arc::new(wes_views::Package::parse(r#"{"name":"EventList","id":"event-list","summary":"Recent events","renderer":"View.tsx","input":"Input","outputs":{},"interaction":null}"#,"types: {Input: {base: Record, fields: {items: 'List<Int>'}}}").unwrap());
    let mut store = Store::new([source.clone(), target.clone()], Limits::default()).unwrap();
    let a = store
        .create(id("source"), "Selector", &source.digest, None)
        .unwrap();
    let b = store
        .create(id("target"), "EventList", &target.digest, None)
        .unwrap();
    store.link(&a, "picked", &b, "items", 0).unwrap();
    assert_eq!(
        store.output(&a, "picked").unwrap().data(),
        &Data::List(vec![])
    );
    let edit = InteractionEdit {
        events: vec![
            EventEmission {
                port: "picked".into(),
                data: Data::Int(7),
            },
            EventEmission {
                port: "picked".into(),
                data: Data::Int(7),
            },
        ],
        owner: a.id().clone(),
        identity: store.read(&a).unwrap().identity.to_string(),
        definition_revision: 0,
        revision: 0,
        fields: [("selection".into(), Data::Int(7))].into(),
        outputs: Default::default(),
    };
    store.commit_interaction(&a, edit.clone()).unwrap();
    assert_eq!(
        store.output(&a, "picked").unwrap().data(),
        &Data::List(vec![Data::Int(7), Data::Int(7)])
    );
    assert_eq!(
        store.commit_interaction(&a, edit.clone()).unwrap_err(),
        Error::Revision
    );
    let mut invalid = edit.clone();
    invalid.revision = 1;
    invalid.events[1].data = Data::Text("wrong".into());
    assert_eq!(
        store.commit_interaction(&a, invalid).unwrap_err(),
        Error::Interaction
    );
    assert_eq!(store.interaction(&a).unwrap().1.revision, 1);
    assert_eq!(
        store.output(&a, "picked").unwrap().data(),
        &Data::List(vec![Data::Int(7), Data::Int(7)])
    );
    for n in 0..64 {
        let mut next = edit.clone();
        next.revision = n + 1;
        next.events = vec![EventEmission {
            port: "picked".into(),
            data: Data::Int(n as i64),
        }];
        store.commit_interaction(&a, next).unwrap();
    }
    let window = store.output(&a, "picked").unwrap();
    let Data::List(items) = window.data() else {
        panic!("event window")
    };
    assert!(items.len() <= 32);
    assert_eq!(items.last(), Some(&Data::Int(63)));
    assert!(
        window
            .provenance()
            .cautions()
            .iter()
            .any(|s| s.contains("omitted"))
    );
    assert_eq!(
        window.shape(),
        &wes_core::Shape::List(Box::new(wes_core::Shape::Primitive(
            wes_core::Primitive::Int
        )))
    );
}
