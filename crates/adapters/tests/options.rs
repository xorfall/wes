use serde_json::json;
use wes_adapters::codec::*;
use wes_core::{Data, Provenance, Shape, Value};

#[test]
fn native_option_preserves_nested_presence_and_neutral_policy() {
    for data in [
        Data::Option(None),
        Data::Option(Some(Box::new(Data::Option(None)))),
        Data::Option(Some(Box::new(Data::Int(0)))),
    ] {
        let value = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
        let bytes = encode_value(&value, Limits::default()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"],
            1
        );
        assert_eq!(
            decode_value(&bytes, Limits::default()).unwrap().value,
            value
        );
    }
    let original = Value::new(Shape::Unknown, Data::Int(7), Provenance::default()).unwrap();
    let bytes = encode_value(&original, Limits::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"],
        1
    );
    assert_eq!(
        decode_value(&bytes, Limits::default()).unwrap().value,
        original
    );
}

#[test]
fn nullable_boundary_preserves_presence_and_request_json_unwraps_options() {
    let raw = decode_json_preserving(
        br#"{"null":null,"zero":0,"false":false,"empty":""}"#,
        Limits::default(),
    )
    .unwrap();
    let Data::Record(fields) = raw else {
        panic!("record")
    };
    assert_eq!(fields.len(), 4);
    assert_eq!(fields["null"], Data::Option(None));
    assert_eq!(fields["zero"], Data::Int(0));
    assert_eq!(fields["false"], Data::Bool(false));
    assert_eq!(fields["empty"], Data::Text("".into()));
    assert!(!fields.contains_key("absent"));
    let mut registry = wes_core::contracts::ContractRegistry::new();
    registry
        .load(r#"{"version":1,"types":{"Row":{"base":"Record","fields":{"x":"Option<Int>"}}}}"#)
        .unwrap();
    let contract = registry.resolve("Row").unwrap();
    let missing = decode_json_for_contract(b"{}", Limits::default(), &contract).unwrap();
    assert!(!contract.issues(&missing).is_empty());
    for (bytes, expected) in [
        (b"{\"x\":null}".as_slice(), Data::Option(None)),
        (
            b"{\"x\":7}".as_slice(),
            Data::Option(Some(Box::new(Data::Int(7)))),
        ),
    ] {
        let data = decode_json_for_contract(bytes, Limits::default(), &contract).unwrap();
        assert!(contract.issues(&data).is_empty());
        let Data::Record(fields) = &data else {
            panic!("record")
        };
        assert_eq!(fields["x"], expected);
        let encoded = encode_request_data(
            &Data::Record([("arg".into(), data)].into()),
            Limits::default(),
        )
        .unwrap();
        let expected: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded).unwrap(),
            json!({"arg": expected})
        );
    }
    assert!(decode_json_preserving(br#"{"x":null,"x":0}"#, Limits::default()).is_err());
}

#[test]
fn option_storage_and_display_are_bounded_and_distinguish_some_none() {
    let value = Value::new(
        Shape::Option(Box::new(Shape::Option(Box::new(Shape::Unknown)))),
        Data::Option(Some(Box::new(Data::Option(None)))),
        Provenance::default(),
    )
    .unwrap();
    let display: serde_json::Value =
        serde_json::from_slice(&encode_display_value(&value, Limits::default()).unwrap()).unwrap();
    assert_eq!(
        display["data"],
        json!({"kind":"some","value":{"kind":"none"}})
    );
    assert!(
        encode_value(
            &value,
            Limits {
                bytes: 16,
                nodes: 100
            }
        )
        .is_err()
    );
    assert!(
        encode_value(
            &value,
            Limits {
                bytes: 4096,
                nodes: 2
            }
        )
        .is_err()
    );
    let mut deep = Data::Int(1);
    for _ in 0..201 {
        deep = Data::Option(Some(Box::new(deep)));
    }
    assert!(
        encode_value(
            &Value::new(Shape::Unknown, deep, Provenance::default()).unwrap(),
            Limits::default()
        )
        .is_err()
    );
}
