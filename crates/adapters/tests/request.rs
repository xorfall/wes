use indexmap::IndexMap;
use serde_json::Value as Json;
use wes_adapters::codec::{Limits, decode_json_for_contract, encode_json, encode_request_data};
use wes_core::{Data, Provenance, Shape, Value};

fn value(data: Data) -> Value {
    Value::new(Shape::Unknown, data, Provenance::default()).unwrap()
}
#[test]
fn acceptance_request_corpus_matches_native_ordered_json() {
    let cases: Vec<Json> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/requests-acceptance.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), 25);
    for case in cases {
        let source = case["json"].as_str().unwrap().as_bytes();
        // These are native request fixture values, not string-to-type response coercions.
        let raw = serde_json::from_slice::<Json>(source).unwrap();
        let data = match case["type"].as_str().unwrap() {
            "Bytes" => Data::Bytes(raw.as_str().unwrap().as_bytes().into()),
            "Instant" => Data::Instant(raw.as_str().unwrap().parse().unwrap()),
            "Duration" => Data::Duration(raw.as_str().unwrap().parse().unwrap()),
            kind => {
                let contract = wes_core::contracts::ContractRegistry::new()
                    .resolve(kind)
                    .unwrap();
                let data = decode_json_for_contract(source, Limits::default(), &contract).unwrap();
                assert!(contract.issues(&data).is_empty());
                data
            }
        };
        let body = encode_request_data(
            &Data::Record([("z".into(), data.clone()), ("a".into(), data)].into()),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            case["body"].as_str().unwrap()
        );
    }
}

#[test]
fn requests_sort_nested_keys_without_mutating_values_or_display_order() {
    let v = value(Data::Record(IndexMap::from_iter([
        ("z".into(), Data::Int(2)),
        ("a".into(), Data::Int(1)),
    ])));
    assert_eq!(
        encode_request_data(v.data(), Limits::default()).unwrap(),
        br#"{"a":1,"z":2}"#
    );
    assert_eq!(
        encode_json(v.data(), Limits::default()).unwrap(),
        br#"{"z":2,"a":1}"#
    );
}

#[test]
fn plain_request_decimals_are_bounded_without_changing_compact_display() {
    for source in ["1e2147483647", "1e-2147483647", "-1e2147483647"] {
        let v = value(Data::Decimal(source.parse().unwrap()));
        let limits = Limits {
            bytes: 128,
            nodes: 10,
        };
        assert!(encode_json(v.data(), limits).is_ok());
        assert!(encode_request_data(v.data(), limits).is_err());
        assert!(
            encode_request_data(
                &Data::Record([("n".into(), v.data().clone())].into()),
                limits
            )
            .is_err()
        );
    }
    let zero = value(Data::Decimal("0e2147483647".parse().unwrap()));
    assert_eq!(
        encode_request_data(zero.data(), Limits { bytes: 1, nodes: 1 }).unwrap(),
        b"0"
    );
}

#[test]
fn request_node_budget_is_shared_across_all_arguments() {
    let arguments = Data::Record(IndexMap::from_iter([
        ("a".into(), Data::Int(1)),
        ("b".into(), Data::Int(2)),
    ]));
    assert!(
        encode_request_data(
            &arguments,
            Limits {
                bytes: 100,
                nodes: 2
            }
        )
        .is_err()
    );
    assert!(
        encode_request_data(
            &arguments,
            Limits {
                bytes: 100,
                nodes: 3
            }
        )
        .is_ok()
    );
    assert!(
        encode_request_data(
            &Data::Record(IndexMap::new()),
            Limits {
                bytes: 100,
                nodes: 0
            }
        )
        .is_err()
    );
}

#[test]
fn aggregate_escaping_and_base64_expansion_obey_encoded_byte_limits() {
    let text = value(Data::Text("\u{0}".repeat(10).into()));
    assert!(
        encode_request_data(
            &Data::Record([("t".into(), text.data().clone())].into()),
            Limits {
                bytes: 30,
                nodes: 2
            }
        )
        .is_err()
    );
    let bytes = value(Data::Bytes(vec![0; 12].into()));
    assert!(
        encode_request_data(
            bytes.data(),
            Limits {
                bytes: 17,
                nodes: 1
            }
        )
        .is_err()
    );
    assert_eq!(
        encode_request_data(
            bytes.data(),
            Limits {
                bytes: 18,
                nodes: 1
            }
        )
        .unwrap()
        .len(),
        18
    );
}

#[test]
fn depth_and_small_scalar_budgets_fail_explicitly() {
    let mut data = Data::Int(1);
    for _ in 0..201 {
        data = Data::List(vec![data]);
    }
    assert!(encode_request_data(&data, Limits::default()).is_err());
    assert!(encode_request_data(&Data::Bool(true), Limits { bytes: 3, nodes: 1 }).is_err());
    assert!(
        encode_request_data(
            &Data::Record(IndexMap::new()),
            Limits { bytes: 1, nodes: 1 }
        )
        .is_err()
    );
}

#[test]
fn request_metadata_is_not_serialized_and_control_characters_round_trip() {
    let v = Value::new(
        Shape::Unknown,
        Data::Text("\u{8}\u{c}\n\r\t\"\\".into()),
        Provenance::default().with_fact("fixture-private", "not-request-data"),
    )
    .unwrap();
    let encoded = encode_request_data(
        &Data::Record([("text".into(), v.data().clone())].into()),
        Limits::default(),
    )
    .unwrap();
    let parsed: Json = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(parsed["text"].as_str().unwrap(), "\u{8}\u{c}\n\r\t\"\\");
    assert!(parsed.get("type").is_none());
    assert!(parsed.get("provenance").is_none());
    assert!(
        !String::from_utf8(encoded)
            .unwrap()
            .contains("not-request-data")
    );
}
