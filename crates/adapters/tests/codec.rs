use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value as Json, json};
use std::{collections::BTreeSet, str::FromStr};
use wes_adapters::codec::*;
use wes_core::{Data, Decimal, Primitive, Provenance, RecordShape, Shape, Value};

fn value(shape: Shape, data: Data) -> Value {
    Value::new(shape, data, Provenance::default()).unwrap()
}

#[test]
fn pretty_display_preserves_exact_numbers_and_respects_output_and_depth_budgets() {
    let data = Data::Record(
        [(
            "epoch".into(),
            Data::Decimal("1790000000000000001".parse().unwrap()),
        )]
        .into(),
    );
    let pretty = encode_json_pretty(&data, Limits::default()).unwrap();
    assert!(String::from_utf8_lossy(&pretty).contains("1790000000000000001"));
    assert!(String::from_utf8_lossy(&pretty).contains('\n'));
    assert_eq!(
        serde_json::from_slice::<Json>(&pretty).unwrap(),
        serde_json::from_slice::<Json>(&encode_json(&data, Limits::default()).unwrap()).unwrap()
    );
    assert!(matches!(
        encode_json_pretty(
            &data,
            Limits {
                bytes: pretty.len() - 1,
                nodes: 100
            }
        ),
        Err(CodecError::Bytes)
    ));
    let mut deep = Data::Int(1);
    for _ in 0..150 {
        deep = Data::List(vec![deep]);
    }
    assert!(encode_json_pretty(&deep, Limits::default()).is_ok());
    for _ in 0..51 {
        deep = Data::List(vec![deep]);
    }
    assert!(matches!(
        encode_json_pretty(&deep, Limits::default()),
        Err(CodecError::Depth)
    ));
}

#[test]
fn shared_large_payloads_preserve_wire_content_and_logical_limits() {
    let text = "x".repeat(10_000);
    let bytes = vec![42; 10_000];
    let original = value(
        Shape::Unknown,
        Data::Record(
            [
                ("text".into(), Data::Text(text.clone().into())),
                ("bytes".into(), Data::Bytes(bytes.clone().into())),
            ]
            .into(),
        ),
    );
    let encoded = encode_json(original.data(), Limits::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&encoded).unwrap(),
        json!({"text":text,"bytes":STANDARD.encode(&bytes)})
    );
    let stored = encode_value(&original, Limits::default()).unwrap();
    assert_eq!(
        decode_value(&stored, Limits::default()).unwrap().value,
        original
    );
    assert!(
        encode_json(
            original.data(),
            Limits {
                bytes: 1024,
                nodes: 100
            }
        )
        .is_err()
    );
}
#[test]
fn historical_retained_value_formats_are_rejected() {
    let cases: Vec<Json> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/values-acceptance.json"
    ))
    .unwrap();
    for case in cases {
        assert!(
            decode_value(
                case["encoded"].as_str().unwrap().as_bytes(),
                Limits::default()
            )
            .is_err()
        );
    }
    let current = encode_value(&value(Shape::Unknown, Data::Int(1)), Limits::default()).unwrap();
    let mut wire: Json = serde_json::from_slice(&current).unwrap();
    assert_eq!(wire["format"], "wes.value");
    assert_eq!(wire["version"], 2);
    let mut old = wire.clone();
    old["format"] = json!("unrelated.value");
    assert!(decode_value(old.to_string().as_bytes(), Limits::default()).is_err());
    for version in [0, 1, 3, 4, 5] {
        wire["version"] = json!(version);
        assert!(decode_value(wire.to_string().as_bytes(), Limits::default()).is_err());
    }
}

#[test]
fn versioned_format_preserves_every_data_kind_below_unknown_and_all_cautions() {
    let data = Data::List(vec![
        Data::Text("hello \"world\"\n".into()),
        Data::Int(i64::MIN),
        Data::Decimal("1E+3".parse().unwrap()),
        Data::Bool(true),
        Data::Instant("-1000000000-01-01T00:00:00Z".parse().unwrap()),
        Data::Duration("PT-0.5S".parse().unwrap()),
        Data::Interval("2025-01-01T00:00:00Z/2025-01-01T00:00:01Z".parse().unwrap()),
        Data::Bytes(vec![0, 255, 10].into()),
        Data::Record([("nested".into(), Data::Int(7))].into()),
    ]);
    let original = value(Shape::Unknown, data).with_provenance(Provenance::new(
        [("quoted\"fact".into(), "value\n\\".into())].into(),
        BTreeSet::from(["unchecked:payload".into(), "inherited".into()]),
    ));
    let written = encode_value(&original, Limits::default()).unwrap();
    let decoded = decode_value(&written, Limits::default()).unwrap();
    assert_eq!(decoded.value, original);
}

#[test]
fn record_names_fields_and_undeclared_data_are_not_lossy_or_unescaped() {
    let shape = Shape::Record(
        RecordShape::new(
            "A\"B\n",
            [("i\"d".into(), Shape::Primitive(Primitive::Int))],
        )
        .unwrap(),
    );
    let original = value(
        shape,
        Data::Record(
            [
                ("i\"d".into(), Data::Int(3)),
                ("attachment".into(), Data::Bytes(vec![65, 66].into())),
            ]
            .into(),
        ),
    );
    let decoded = decode_value(
        &encode_value(&original, Limits::default()).unwrap(),
        Limits::default(),
    )
    .unwrap()
    .value;
    assert_eq!(decoded, original);
    let Shape::Record(record) = decoded.shape() else {
        panic!("record")
    };
    assert_eq!(record.name(), "A\"B\n");
}

#[test]
fn extreme_decimals_stay_compact_in_json_and_preserve_scale_in_storage() {
    for text in [
        "1e2147483647",
        "1e-2147483647",
        "0E+2147483648",
        "12.3400",
        "1E+3",
        "-0.00",
    ] {
        let number = Decimal::from_str(text).unwrap();
        let original = value(Shape::Primitive(Primitive::Decimal), Data::Decimal(number));
        let json = encode_json(original.data(), Limits::default()).unwrap();
        assert!(json.len() < 32);
        assert!(serde_json::from_slice::<&serde_json::value::RawValue>(&json).is_ok());
        let bytes = encode_value(&original, Limits::default()).unwrap();
        assert!(bytes.len() < 300);
        assert_eq!(
            decode_value(&bytes, Limits::default()).unwrap().value,
            original
        );
    }
}

#[test]
fn output_and_input_limits_fail_explicitly_without_truncating_values() {
    let original = value(
        Shape::Primitive(Primitive::Text),
        Data::Text("\"".repeat(100).into()),
    );
    let full = encode_value(&original, Limits::default()).unwrap();
    let exact = Limits {
        bytes: full.len(),
        ..Limits::default()
    };
    assert_eq!(encode_value(&original, exact).unwrap(), full);
    assert!(matches!(
        encode_value(
            &original,
            Limits {
                bytes: full.len() - 1,
                ..exact
            }
        ),
        Err(CodecError::Bytes)
    ));
    assert!(matches!(
        decode_value(
            &full,
            Limits {
                bytes: full.len() - 1,
                ..exact
            }
        ),
        Err(CodecError::Bytes)
    ));
    let list = value(Shape::Unknown, Data::List((0..20).map(Data::Int).collect()));
    let bytes = encode_value(&list, Limits::default()).unwrap();
    assert!(
        encode_value(
            &list,
            Limits {
                nodes: 10,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        decode_value(
            &bytes,
            Limits {
                nodes: 10,
                ..Limits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn nested_values_beyond_serde_default_recursion_limit_are_supported_but_bounded() {
    let mut data = Data::Int(1);
    for _ in 0..150 {
        data = Data::List(vec![data]);
    }
    let original = value(Shape::Unknown, data);
    let bytes = encode_value(&original, Limits::default()).unwrap();
    assert_eq!(
        decode_value(&bytes, Limits::default()).unwrap().value,
        original
    );
    let deep = format!(
        "{{\"format\":\"wes.value\",\"version\":2,\"type\":{{\"kind\":\"unknown\"}},\"data\":{}1{}}}",
        "{\"kind\":\"list\",\"value\":[".repeat(10_000),
        "]}".repeat(10_000)
    );
    assert!(matches!(
        decode_value(deep.as_bytes(), Limits::default()),
        Err(CodecError::Depth)
    ));
}

#[test]
fn duplicate_keys_invalid_unicode_and_unknown_versions_are_rejected() {
    for input in [
        r#"{"type":{"kind":"unknown"},"type":{"kind":"unknown"},"data":1}"#,
        r#"{"type":{"kind":"unknown"},"data":{"same":1,"same":2}}"#,
        r#"{"type":{"kind":"unknown"},"data":"\ud800"}"#,
        r#"{"format":"wes.value","version":999,"type":{"kind":"unknown"},"data":1}"#,
        r#"{"format":"foreign","version":1,"type":{"kind":"unknown"},"data":1}"#,
    ] {
        assert!(
            decode_value(input.as_bytes(), Limits::default()).is_err(),
            "{input}"
        );
    }
}

#[test]
fn serde_private_keys_remain_ordinary_user_data() {
    let original = value(
        Shape::Unknown,
        Data::Record(
            [
                (
                    "$serde_json::private::Number".into(),
                    Data::Text("999".into()),
                ),
                (
                    "$serde_json::private::RawValue".into(),
                    Data::Text("false".into()),
                ),
            ]
            .into(),
        ),
    );
    assert_eq!(
        decode_value(
            &encode_value(&original, Limits::default()).unwrap(),
            Limits::default()
        )
        .unwrap()
        .value,
        original
    );
}

#[test]
fn malformed_lists_and_inconsistent_stored_roots_are_not_silently_repaired() {
    for input in [
        r#"{"type":{"kind":"list","element":{"kind":"unknown"}},"data":"not a list"}"#,
        r#"{"type":{"kind":"primitive","name":"TEXT"},"data":7}"#,
    ] {
        assert!(decode_value(input.as_bytes(), Limits::default()).is_err());
    }
}

#[test]
fn node_budget_counts_shape_data_and_provenance_consistently() {
    let original = value(Shape::Primitive(Primitive::Int), Data::Int(1)).with_provenance(
        Provenance::default()
            .with_fact("origin", "example")
            .cautioned(["warning".into()]),
    );
    let limits = Limits {
        nodes: 4,
        ..Limits::default()
    };
    let bytes = encode_value(&original, limits).unwrap();
    assert_eq!(decode_value(&bytes, limits).unwrap().value, original);
    assert!(matches!(
        encode_value(&original, Limits { nodes: 3, ..limits }),
        Err(CodecError::Work)
    ));
}

#[test]
fn browser_envelope_preserves_structural_types_facts_and_display_data_without_storage_tags() {
    let shape = Shape::Record(
        RecordShape::new(
            "Row",
            [("payload".into(), Shape::Primitive(Primitive::Bytes))],
        )
        .unwrap(),
    );
    let original = value(
        shape,
        Data::Record([("payload".into(), Data::Bytes(vec![0, 255].into()))].into()),
    )
    .with_provenance(Provenance::new(
        [("source".into(), "fixture".into())].into(),
        ["unchecked:fixture".into()].into(),
    ));
    let encoded = encode_display_value(&original, Limits::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&encoded).unwrap(),
        json!({"type":{"kind":"record","name":"Row","fields":[{"name":"payload","type":{"kind":"primitive","name":"BYTES"}}]},"provenance":{"source":"fixture"},"data":{"payload":"AP8="}})
    );
    assert_eq!(
        encode_display_value(
            &original,
            Limits {
                bytes: encoded.len(),
                ..Limits::default()
            }
        )
        .unwrap(),
        encoded
    );
    assert!(matches!(
        encode_display_value(
            &original,
            Limits {
                bytes: encoded.len() - 1,
                ..Limits::default()
            }
        ),
        Err(CodecError::Bytes)
    ));
    assert!(matches!(
        encode_display_value(
            &original,
            Limits {
                nodes: 1,
                ..Limits::default()
            }
        ),
        Err(CodecError::Work)
    ));
    // Storage remains lossless/versioned and retains cautions separately from this browser envelope.
    assert_eq!(
        decode_value(
            &encode_value(&original, Limits::default()).unwrap(),
            Limits::default()
        )
        .unwrap()
        .value,
        original
    );
}

#[test]
fn retained_iter_is_self_contained_versioned_and_display_never_exposes_source() {
    use wes_core::{IterMode, IterStage, IterValue};
    let source = value(
        Shape::Primitive(Primitive::Text),
        Data::Text("secret snapshot\r\nsecond".into()),
    );
    let iter = IterValue::new(source, IterMode::Lines, None, vec![IterStage::Take(1)]).unwrap();
    let v = value(
        Shape::Iter(Box::new(Shape::Primitive(Primitive::Text))),
        Data::Iter(std::sync::Arc::new(iter)),
    );
    let bytes = encode_value(&v, Limits::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&bytes).unwrap()["version"],
        2
    );
    assert_eq!(decode_value(&bytes, Limits::default()).unwrap().value, v);
    let display = String::from_utf8(encode_display_value(&v, Limits::default()).unwrap()).unwrap();
    assert!(!display.contains("secret"));
    assert!(!display.contains("second"));
    assert!(display.contains("iter"));
    assert!(
        encode_request_data(
            &Data::Record([("payload".into(), v.data().clone())].into()),
            Limits::default()
        )
        .is_err()
    );
    assert!(encode_request_data(v.data(), Limits::default()).is_err());
    let mut bad: Json = serde_json::from_slice(&bytes).unwrap();
    bad["version"] = json!(1);
    assert!(decode_value(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    let nested = value(
        Shape::Unknown,
        Data::Option(Some(Box::new(v.data().clone()))),
    );
    assert_eq!(
        serde_json::from_slice::<Json>(&encode_value(&nested, Limits::default()).unwrap()).unwrap()
            ["version"],
        2
    );
    assert!(
        encode_request_data(
            &Data::Record([("nested".into(), nested.data().clone())].into()),
            Limits::default()
        )
        .is_err()
    );
}

#[test]
fn retained_iter_budget_includes_source_provenance_item_type_and_captured_packages() {
    use wes_core::{IterMode, IterStage, IterValue, contracts::ContractRegistry};
    let mut registry = ContractRegistry::new();
    registry
        .load("types:\n  Line: {base: Text, maxLength: 20}\n")
        .unwrap();
    let source = value(
        Shape::Primitive(Primitive::Text),
        Data::Text("hello".into()),
    )
    .with_provenance(
        Provenance::default()
            .with_fact("source", "fixture")
            .cautioned(["source caution".into()]),
    );
    let iter = IterValue::new(
        source,
        IterMode::Lines,
        None,
        vec![
            IterStage::Check(registry.capture("Line").unwrap()),
            IterStage::Take(1),
        ],
    )
    .unwrap();
    let original = value(
        Shape::Iter(Box::new(iter.item_shape().clone())),
        Data::Iter(std::sync::Arc::new(iter)),
    )
    .with_provenance(
        Provenance::default()
            .with_fact("output", "fixture")
            .cautioned(["output caution".into()]),
    );
    // Outer shape (2), Iter data (1), source shape/data/provenance (4),
    // item shape (1), stages (2), captured package (1), outer provenance (2).
    let limits = Limits {
        nodes: 13,
        ..Limits::default()
    };
    let bytes = encode_value(&original, limits).unwrap();
    assert_eq!(decode_value(&bytes, limits).unwrap().value, original);
    assert!(matches!(
        encode_value(
            &original,
            Limits {
                nodes: 12,
                ..limits
            }
        ),
        Err(CodecError::Work)
    ));
    assert!(
        decode_value(
            &bytes,
            Limits {
                nodes: 12,
                ..limits
            }
        )
        .is_err()
    );
}

#[test]
fn retained_depth_budget_is_symmetric_at_the_value_and_iter_source_boundaries() {
    use wes_core::{IterMode, IterValue};
    for source_depth in [0, 1] {
        let mut source = Data::Text("leaf".into());
        for _ in 0..source_depth {
            source = Data::List(vec![source]);
        }
        let iter = IterValue::new(
            value(Shape::Unknown, source),
            if source_depth == 0 {
                IterMode::Lines
            } else {
                IterMode::Items
            },
            None,
            vec![],
        )
        .unwrap();
        let mut data = Data::Iter(std::sync::Arc::new(iter));
        for _ in 0..199 - source_depth {
            data = Data::List(vec![data]);
        }
        let original = value(Shape::Unknown, data.clone());
        let bytes = encode_value(&original, Limits::default()).unwrap();
        assert_eq!(
            decode_value(&bytes, Limits::default()).unwrap().value,
            original
        );
        let too_deep = value(Shape::Unknown, Data::List(vec![data]));
        assert!(matches!(
            encode_value(&too_deep, Limits::default()),
            Err(CodecError::Depth)
        ));
    }
    let mut data = Data::Int(1);
    for _ in 0..200 {
        data = Data::List(vec![data]);
    }
    let original = value(Shape::Unknown, data);
    let bytes = encode_value(&original, Limits::default()).unwrap();
    assert_eq!(
        decode_value(&bytes, Limits::default()).unwrap().value,
        original
    );
}

#[test]
fn stored_management_projection_restores_type_but_never_live_authority() {
    let plan = wes_core::Value::management(
        wes_core::MetaType::WorkspaceDeletePlan,
        wes_core::Data::Record(Default::default()),
        "synthetic-authority".into(),
    );
    let bytes =
        wes_adapters::codec::encode_value(&plan, wes_adapters::codec::Limits::default()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-authority"));
    let restored =
        wes_adapters::codec::decode_value(&bytes, wes_adapters::codec::Limits::default())
            .unwrap()
            .value;
    assert_eq!(restored.shape(), plan.shape());
    assert_eq!(restored.data(), plan.data());
    assert!(restored.management_authority().is_none());
}

#[test]
fn interval_retained_codec_validates_invariant_and_keeps_its_nominal_kind() {
    let original = value(
        Shape::Primitive(Primitive::Interval),
        Data::Interval(
            "2025-01-01T00:00:00.000000001Z/2025-01-01T00:00:00.000000002Z"
                .parse()
                .unwrap(),
        ),
    );
    let encoded = encode_value(&original, Limits::default()).unwrap();
    assert_eq!(
        decode_value(&encoded, Limits::default()).unwrap().value,
        original
    );
    let text = String::from_utf8(encoded).unwrap();
    let reversed = text.replace(
        "2025-01-01T00:00:00.000000001Z/2025-01-01T00:00:00.000000002Z",
        "2025-01-01T00:00:00.000000002Z/2025-01-01T00:00:00.000000001Z",
    );
    assert_ne!(text, reversed);
    assert!(decode_value(reversed.as_bytes(), Limits::default()).is_err());
}

#[test]
fn public_iterator_metadata_preserves_declared_contract_without_exporting_source() {
    use wes_core::{ContractCapture, IterMode, IterStage, IterValue};
    let source = value(Shape::Unknown, Data::Text("synthetic input".into()));
    let capture = ContractCapture {
        name: "EntryLine".into(),
        packages: vec!["types:\n  EntryLine:\n    base: Text\n    pattern: '^entry:'\n".into()],
    };
    let iter = IterValue::new(
        source,
        IterMode::Lines,
        None,
        vec![IterStage::Check(capture)],
    )
    .unwrap();
    let v = value(
        Shape::Iter(Box::new(Shape::Primitive(Primitive::Text))),
        Data::Iter(std::sync::Arc::new(iter)),
    );
    let display: Json =
        serde_json::from_slice(&encode_display_value(&v, Limits::default()).unwrap()).unwrap();
    assert_eq!(display["data"]["itemContract"], "EntryLine");
    assert_eq!(display["data"]["itemType"]["name"], "TEXT");
    assert!(!display.to_string().contains("synthetic input"));
    assert_eq!(
        decode_value(
            &encode_value(&v, Limits::default()).unwrap(),
            Limits::default()
        )
        .unwrap()
        .value,
        v
    );
}

#[test]
fn human_numbers_remove_padding_without_changing_transport_or_stored_scale() {
    let number = Data::Decimal("90.000000000".parse().unwrap());
    let data = Data::Record(
        [
            ("value".into(), number.clone()),
            (
                "nested".into(),
                Data::List(vec![Data::Option(Some(Box::new(Data::Decimal(
                    "1.500000".parse().unwrap(),
                ))))]),
            ),
            ("text".into(), Data::Text("90.000000000".into())),
        ]
        .into(),
    );
    let human =
        String::from_utf8(encode_json_human(&data, Limits::default(), false).unwrap()).unwrap();
    assert!(human.contains("\"value\":90"));
    assert!(human.contains("\"value\":1.5"));
    assert!(human.contains("\"text\":\"90.000000000\""));
    assert_eq!(
        encode_json(&number, Limits::default()).unwrap(),
        b"90.000000000"
    );
    assert!(matches!(
        encode_json_human(
            &data,
            Limits {
                bytes: 2,
                nodes: 100
            },
            false
        ),
        Err(CodecError::Bytes)
    ));
}

#[test]
fn captured_metadata_round_trips_without_registry_and_rejects_malformed_inputs() {
    use wes_core::contracts::{ContractRegistry, boundary};
    let mut r = ContractRegistry::new();
    r.load("version: 2\ntypes: {Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: ok, failed: bad}}}, Row: {base: Record, fields: {status: Status}}}").unwrap();
    let contract = r.resolve("List<Row>").unwrap();
    let v = value(
        Shape::Unknown,
        Data::List(vec![Data::Record(
            [("status".into(), Data::Text("ready".into()))].into(),
        )]),
    );
    let v = boundary::checked_result(&contract, &v, &|| false).unwrap();
    let stored = encode_value(&v, Limits::default()).unwrap();
    drop(r);
    let mut today = wes_core::contracts::ContractRegistry::new();
    today.load("version: 2\ntypes: {Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: warn, failed: bad}}}, Row: {base: Record, fields: {status: Status}}}").unwrap();
    let restored = decode_value(&stored, Limits::default()).unwrap().value;
    let yesterday = serde_json::to_value(restored.metadata().unwrap()).unwrap();
    let current = serde_json::to_value(wes_core::contracts::metadata::ValueMetadata::capture(
        &today.resolve("List<Row>").unwrap(),
    ))
    .unwrap();
    assert_ne!(
        yesterday["contract"]["digest"],
        current["contract"]["digest"]
    );
    assert_eq!(yesterday["fields"]["/e/f:status"]["tones"]["ready"], "ok");
    assert_eq!(decode_value(&stored, Limits::default()).unwrap().value, v);
    let display: Json =
        serde_json::from_slice(&encode_display_value(&v, Limits::default()).unwrap()).unwrap();
    assert_eq!(
        display["meta"]["fields"]["/e/f:status"]["members"],
        json!(["ready", "failed"])
    );
    // These are exactly the kinds accepted by the GUI's decodeMeta.
    assert!(
        display["meta"]["fields"]
            .as_object()
            .unwrap()
            .values()
            .all(|d| {
                matches!(
                    d["kind"].as_str(),
                    Some("text" | "int" | "decimal" | "bool")
                )
            })
    );
    assert_eq!(display["meta"].as_object().unwrap().len(), 4);
    assert!(display.get("metaProjection").is_none());
    assert_eq!(
        restored
            .metadata()
            .unwrap()
            .project("/e")
            .unwrap()
            .wire()
            .unwrap(),
        v.metadata().unwrap().project("/e").unwrap().wire().unwrap()
    );
    println!("CAPTURED_WIRE={}", display);
    let original: Json = serde_json::from_slice(&stored).unwrap();
    for (pointer, bad) in [
        ("/meta/version", json!(2)),
        ("/meta/fields/~1e~1f:status/tones/ready", json!("inherit")),
        ("/meta/fields/~1e~1f:status/tones", json!({"outside":"bad"})),
        ("/meta/contract/digest", json!("sha256:bad")),
        ("/meta/fields/~1e~1f:status/kind", json!("colour")),
        ("/meta/fields/~1e~1f:status/kind", json!("list")),
        (
            "/meta/fields/~1e~1f:status/total",
            json!(9_007_199_254_740_992_u64),
        ),
        ("/meta/fields/~1e~1f:status/source", json!("observed")),
        ("/meta/fields/~1e~1f:status/total", json!(1)),
        (
            "/meta/fields/~1e~1f:status/members",
            json!(["ready", "ready"]),
        ),
        ("/meta/fields/~1e~1f:status/complete", json!(false)),
        ("/meta/fields/~1e~1f:status/extra", json!(true)),
    ] {
        let mut bad_wire = original.clone();
        if pointer.ends_with("/extra") {
            bad_wire["meta"]["fields"]["/e/f:status"]["extra"] = bad;
        } else {
            *bad_wire.pointer_mut(pointer).unwrap() = bad;
        }
        assert!(
            decode_value(&serde_json::to_vec(&bad_wire).unwrap(), Limits::default()).is_err(),
            "{pointer}"
        );
    }
    for path in ["/status", "/f:x~2", "/e/0", "relative", "/f:wrong"] {
        let mut bad = original.clone();
        let descriptor = bad["meta"]["fields"]["/e/f:status"].clone();
        bad["meta"]["fields"][path] = descriptor;
        assert!(decode_value(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    }
    for bad_projection in [Json::Null, original["meta"].clone()] {
        let mut bad = original.clone();
        bad["metaProjection"] = bad_projection;
        assert!(decode_value(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    }
    let mut nulls = original.clone();
    for key in ["source", "members", "total", "complete", "tones"] {
        nulls["meta"]["fields"]["/e/f:status"][key] = Json::Null;
    }
    assert!(decode_value(&serde_json::to_vec(&nulls).unwrap(), Limits::default()).is_err());
    let duplicated = String::from_utf8(stored).unwrap().replace(
        "\"truncated\":false",
        "\"truncated\":false,\"truncated\":false",
    );
    assert!(decode_value(duplicated.as_bytes(), Limits::default()).is_err());
}
