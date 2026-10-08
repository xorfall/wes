use wes_adapters::codec::{CodecError, Limits, decode_value, encode_value};
use wes_core::{
    Data, Provenance, Shape, Value,
    contracts::{ContractRegistry, boundary},
};

#[test]
fn retained_metadata_node_budget_is_symmetric_and_refuses_one_node_less() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Status: {base: Text, enum: [ready, failed]}} ")
        .unwrap();
    let raw = Value::new(
        Shape::Unknown,
        Data::Text("ready".into()),
        Provenance::default(),
    )
    .unwrap();
    let value =
        boundary::checked_result(&registry.resolve("Status").unwrap(), &raw, &|| false).unwrap();
    // One primitive shape and one tagged scalar, plus the captured metadata JSON nodes.
    let nodes = value.metadata().unwrap().nodes() + 2;
    let limits = Limits {
        bytes: 16 * 1024,
        nodes,
    };
    let encoded = encode_value(&value, limits).unwrap();
    assert_eq!(decode_value(&encoded, limits).unwrap().value, value);
    let smaller = Limits {
        nodes: nodes - 1,
        ..limits
    };
    assert!(matches!(
        encode_value(&value, smaller),
        Err(CodecError::Work)
    ));
    assert!(matches!(
        decode_value(&encoded, smaller),
        Err(CodecError::Work)
    ));
}

#[test]
fn container_projection_snapshot_is_bounded_charged_and_restored() {
    let mut registry = ContractRegistry::new();
    registry.load("types: {Status: {base: Text, enum: [ready, failed]}, Row: {base: Record, fields: {status: Status}}}").unwrap();
    let raw = Value::new(Shape::Unknown, Data::List(vec![]), Provenance::default()).unwrap();
    let value =
        boundary::checked_result(&registry.resolve("List<Row>").unwrap(), &raw, &|| false).unwrap();
    // List + record + scalar shape nodes, plus empty tagged list and both descriptions.
    let limits = Limits {
        bytes: 64 * 1024,
        nodes: value.metadata().unwrap().nodes() + 4,
    };
    let encoded = encode_value(&value, limits).unwrap();
    let restored = decode_value(&encoded, limits).unwrap().value;
    assert_eq!(restored, value);
    assert_eq!(
        restored.metadata().unwrap().project("/e"),
        value.metadata().unwrap().project("/e")
    );
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(wire["meta"]["fields"].as_object().unwrap().len(), 1);
    assert!(wire.get("metaProjection").is_some());
    let smaller = Limits {
        nodes: limits.nodes - 1,
        ..limits
    };
    assert!(matches!(
        encode_value(&value, smaller),
        Err(CodecError::Work)
    ));
    assert!(matches!(
        decode_value(&encoded, smaller),
        Err(CodecError::Work)
    ));
}
