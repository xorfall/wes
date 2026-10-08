use std::sync::Arc;
use wes_core::{Data, IterMode, IterValue, Provenance, Shape, Value, flow::FlowPolicy};

#[test]
fn private_remote_uncertainty_keeps_only_its_safe_classification() {
    let error = wes_core::ErrorValue::new(
        wes_core::ErrorId::new("uncertain").unwrap(),
        "ENV036",
        "qa-private-payload",
        vec![],
        None,
    )
    .unwrap()
    .with_policy(&FlowPolicy::default().from_origin("dev").private());
    assert_eq!(error.code(), "ENV036");
    assert!(error.message().contains("remote work may still be running"));
    assert!(!format!("{error:?}").contains("qa-private-payload"));
    assert!(error.to_value().provenance().policy().is_private());
}

#[test]
fn flow_join_is_monotone_even_when_descriptive_facts_disagree() {
    let a = Provenance::default()
        .with_fact("source", "a")
        .with_policy(&FlowPolicy::default().from_origin("dev").private());
    let b = Provenance::default()
        .with_fact("source", "b")
        .with_policy(&FlowPolicy::default().from_origin("prod"));
    let joined = Provenance::agreed_by([&a, &b]);
    assert_eq!(joined.fact("source"), None);
    assert!(joined.policy().is_private());
    assert_eq!(joined.policy().origins().len(), 2);
    assert_eq!(a.policy().join(b.policy()), b.policy().join(a.policy()));
    let value = Value::new(
        Shape::Unknown,
        Data::Text("qa-private-sentinel".into()),
        joined,
    )
    .unwrap();
    assert!(
        value
            .with_provenance(Provenance::default())
            .provenance()
            .policy()
            .is_private()
    );
    assert!(!format!("{value:?}").contains("qa-private-sentinel"));
}
#[test]
fn bounded_labels_fail_closed_and_nested_iter_cannot_drop_policy() {
    let mut policy = FlowPolicy::default();
    for i in 0..129 {
        policy = policy.from_origin(format!("env-{i}"));
    }
    assert!(policy.is_unknown());
    assert_eq!(policy.origins().len(), 128);
    let source = Value::new(
        Shape::Unknown,
        Data::Text("qa-private-source".into()),
        Provenance::default().with_policy(&FlowPolicy::default().private().from_origin("prod")),
    )
    .unwrap();
    let iter = IterValue::new(source, IterMode::Lines, None, vec![]).unwrap();
    let wrapped = Value::new(
        Shape::Unknown,
        Data::List(vec![Data::Iter(Arc::new(iter))]),
        Provenance::default(),
    )
    .unwrap();
    assert!(wrapped.provenance().policy().is_private());
    assert!(wrapped.provenance().policy().origins().contains("prod"));
}
#[test]
fn private_error_ports_keep_restrictions_and_remove_payload_from_diagnostics() {
    let error = wes_core::ErrorValue::new(
        wes_core::ErrorId::new("qa-error").unwrap(),
        "ERR",
        "qa-private-error",
        vec![],
        None,
    )
    .unwrap()
    .with_policy(&FlowPolicy::default().private().from_origin("prod"));
    assert!(!error.message().contains("qa-private-error"));
    assert!(error.to_value().provenance().policy().is_private());
    assert!(
        error
            .to_cancellation_value()
            .provenance()
            .policy()
            .origins()
            .contains("prod")
    );
}

#[test]
fn confidentiality_and_residence_join_without_granting_retention() {
    use wes_core::flow::Residence;
    let retained = FlowPolicy::default()
        .confidential(Residence::Retainable)
        .from_origin("api");
    let temporary = FlowPolicy::default()
        .confidential(Residence::Temporary)
        .from_origin("capture");
    let memory = FlowPolicy::default().private();
    assert!(retained.is_confidential());
    assert!(!retained.is_private());
    assert!(retained.allows_retention());
    assert_eq!(retained.join(&temporary).residence(), Residence::Temporary);
    assert!(!retained.join(&temporary).allows_retention());
    assert!(temporary.join(&memory).is_private());
    assert!(retained.join(&memory).is_private());
    assert!(
        memory
            .clone()
            .confidential(Residence::Retainable)
            .is_private()
    );
    for a in [
        &retained,
        &temporary,
        &memory,
        &FlowPolicy::default().unknown(),
    ] {
        for b in [&retained, &temporary, &memory] {
            assert_eq!(a.join(b), b.join(a));
            assert_eq!(a.join(a), *a);
        }
    }
}

#[test]
fn confidential_derivatives_and_error_details_do_not_lose_protection() {
    use wes_core::flow::Residence;
    let policy = FlowPolicy::default()
        .confidential(Residence::Temporary)
        .from_origin("fixture");
    let value = Value::new(
        Shape::Unknown,
        Data::Text("confidential-sentinel".into()),
        Provenance::default().with_policy(&policy),
    )
    .unwrap();
    let derived = value.with_provenance(Provenance::default());
    assert_eq!(derived.provenance().policy(), &policy);
    assert!(!format!("{derived:?}").contains("confidential-sentinel"));
    let error = wes_core::ErrorValue::new(
        wes_core::ErrorId::new("confidential-error").unwrap(),
        "ERR",
        "confidential-sentinel",
        vec![],
        None,
    )
    .unwrap()
    .with_policy(&policy);
    assert!(!error.message().contains("confidential-sentinel"));
    assert_eq!(error.to_value().provenance().policy(), &policy);
}
