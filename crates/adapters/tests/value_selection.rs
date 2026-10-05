use indexmap::IndexMap;
use serde_json::{Value as Json, json};
use wes_adapters::codec::{Limits, ValueSelection, decode_value, encode_selection};
use wes_core::{Data, Primitive, Provenance, Shape, Value};

fn read(value: &Value, request: Json, typed: bool) -> Result<Json, String> {
    let selection: ValueSelection = serde_json::from_value(request).unwrap();
    encode_selection(
        value,
        &selection,
        typed,
        Limits {
            bytes: 1024,
            nodes: 100,
        },
    )
    .map(|b| serde_json::from_slice(&b).unwrap())
    .map_err(|e| e.to_string())
}
fn fixture() -> Value {
    Value::new(
        Shape::Unknown,
        Data::Record(IndexMap::from([
            (
                "large".into(),
                Data::Text("x".repeat(9 * 1024 * 1024).into()),
            ),
            (
                "rows".into(),
                Data::List((0..10_000).map(Data::Int).collect()),
            ),
            ("a/b~c".into(), Data::Int(i64::MAX)),
            ("".into(), Data::Bool(true)),
        ])),
        Provenance::default(),
    )
    .unwrap()
}
#[test]
fn tiny_pages_and_metadata_do_not_scan_or_serialize_large_unselected_bodies() {
    let value = fixture();
    assert_eq!(
        read(
            &value,
            json!({"select":"/rows","offset":9998,"limit":2}),
            false
        )
        .unwrap(),
        json!({"value":[9998,9999],"page":{"offset":9998,"count":2,"total":10000,"next_offset":null}})
    );
    let head = read(&value, json!({"select":"/rows","limit":2}), false).unwrap();
    assert_eq!(head["page"]["next_offset"], 2);
    let shape = read(&value, json!({"select":"/rows","shape_only":true}), false).unwrap();
    assert_eq!(shape["length"], 10000);
    assert_eq!(shape["kind"], "list");
    assert_eq!(shape, json!({"kind":"list", "length":10000}));
    assert!(
        read(&value, json!({"select":"/rows", "shape_only":true}), true)
            .unwrap()
            .get("type")
            .is_some()
    );
    assert!(shape.get("value").is_none());
    assert_eq!(
        read(&value, json!({"select":"/rows","offset":10000}), false).unwrap()["value"],
        json!([])
    );
    assert!(
        read(&value, json!({"select":"/large"}), false)
            .unwrap_err()
            .contains("byte budget")
    );
}
#[test]
fn pointers_and_invalid_ranges_have_one_unambiguous_contract() {
    let value = fixture();
    assert_eq!(
        read(&value, json!({"select":"/"}), false).unwrap()["value"],
        true
    );
    assert_eq!(
        read(&value, json!({"select":"/a~1b~0c"}), false).unwrap()["value"],
        i64::MAX
    );
    assert_eq!(
        read(&value, json!({"select":"/rows/3"}), false).unwrap()["value"],
        3
    );
    for request in [
        json!({"select":"rows"}),
        json!({"select":"/~x"}),
        json!({"select":"/rows/01"}),
        json!({"select":"/rows/-"}),
        json!({"select":"/missing"}),
        json!({"select":"/rows/0/x"}),
        json!({"select":"/rows","limit":0}),
        json!({"select":"/rows","limit":1001}),
        json!({"select":"/rows","offset":10001}),
        json!({"select":"/large","limit":1}),
        json!({"shape_only":true,"offset":0}),
    ] {
        let error = read(&value, request.clone(), false).unwrap_err();
        assert!(
            error.starts_with("invalid value selection: "),
            "{request}: {error}"
        );
        assert!(!error.contains("stored data"), "{error}");
    }
}
#[test]
fn typed_selection_roundtrips_exact_scalar_kind_and_page_shape() {
    let value = Value::new(
        Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
        Data::List(vec![Data::Int(i64::MAX), Data::Int(1)]),
        Provenance::default(),
    )
    .unwrap();
    for (request, expected) in [
        (json!({"select":"/0"}), Data::Int(i64::MAX)),
        (
            json!({"offset":0,"limit":1}),
            Data::List(vec![Data::Int(i64::MAX)]),
        ),
    ] {
        let result = read(&value, request, true).unwrap();
        let decoded = decode_value(
            &serde_json::to_vec(&result["value"]).unwrap(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(decoded.value.data(), &expected);
        assert_ne!(decoded.value.shape(), &Shape::Unknown);
    }
}
#[test]
fn privacy_precedes_path_errors_paging_and_metadata() {
    for policy in [
        wes_core::flow::FlowPolicy::default().private(),
        wes_core::flow::FlowPolicy::default().unknown(),
    ] {
        let value = fixture().with_provenance(Provenance::default().with_policy(&policy));
        for request in [
            json!({"shape_only":true}),
            json!({"select":"/missing"}),
            json!({"select":"/rows","limit":1}),
        ] {
            for typed in [false, true] {
                assert_eq!(
                    read(&value, request.clone(), typed).unwrap_err(),
                    "value export refused: This value cannot be exported to terminal processes."
                );
            }
        }
    }
}

#[test]
fn projection_never_consumes_lazy_recipes_and_preserves_unknown_root_policy() {
    use std::sync::Arc;
    use wes_core::{IterMode, IterValue};
    let source = Value::new(
        Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
        Data::List(vec![Data::Int(1)]),
        Provenance::default(),
    )
    .unwrap();
    let recipe = IterValue::new(source, IterMode::Items, None, vec![]).unwrap();
    let data = Data::Record(IndexMap::from([
        ("lazy".into(), Data::Iter(Arc::new(recipe))),
        ("small".into(), Data::Int(42)),
    ]));
    let value = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
    assert!(
        read(&value, json!({"select":"/lazy"}), false)
            .unwrap_err()
            .contains("Materialize")
    );
    assert!(
        read(&value, json!({"select":"/lazy","shape_only":true}), false)
            .unwrap_err()
            .contains("Materialize")
    );
    assert_eq!(
        read(&value, json!({"select":"/small"}), false).unwrap()["value"],
        42
    );
    let unknown = value.with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
    );
    assert!(
        read(&unknown, json!({"select":"/small"}), false)
            .unwrap_err()
            .contains("cannot be exported")
    );
}
