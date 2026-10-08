use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value as Json, json};
use wes_adapters::codec::{Limits, decode_json_for_contract, decode_json_preserving};
use wes_core::{Data, contracts::ContractRegistry};
fn registry() -> ContractRegistry {
    let mut registry = ContractRegistry::new();
    registry
        .load(
            r#"{"version":1,"types":{
        "Item":{"base":"Record","fields":{"id":"Int","active":"Bool"}},
        "Envelope":{"base":"Record","fields":{"items":"List<Item>"}},
        "Escaped":{"base":"Record","fields":{"a/b~":"Int"}},
        "Metadata":{"base":"Record","fields":{"count":"Text"}},
        "WithMetadata":{"base":"Record","fields":{"metadata":"Metadata"}}
    }}"#,
        )
        .unwrap();
    registry
}
fn fingerprint(data: &Data) -> Json {
    let (kind, body) = match data {
        Data::Iter(_) => panic!("foreign fixtures cannot produce native Iter"),
        Data::Dataset(_) => panic!("foreign fixtures cannot produce an owned Dataset reference"),
        Data::Option(value) => (
            "Option",
            value.as_deref().map(fingerprint).unwrap_or(Json::Null),
        ),
        Data::Text(text) => ("Text", json!(text.as_ref())),
        Data::Int(n) => ("Int", json!(n.to_string())),
        Data::Decimal(n) => ("Decimal", json!(n.to_string())),
        Data::Bool(b) => ("Bool", json!(b)),
        Data::Instant(t) => ("Moment", json!(t.to_string())),
        Data::Duration(t) => ("Length", json!(t.to_string())),
        Data::Interval(t) => ("Interval", json!(t.to_string())),
        Data::Bytes(bytes) => ("Bytes", json!(STANDARD.encode(bytes))),
        Data::List(items) => (
            "Sequence",
            Json::Array(items.iter().map(fingerprint).collect()),
        ),
        Data::Record(fields) => (
            "Fields",
            Json::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), fingerprint(value)))
                    .collect(),
            ),
        ),
    };
    json!({"kind":kind,"value":body})
}

#[test]
fn acceptance_response_corpus_preserves_data_without_scalar_coercion() {
    let cases: Vec<Json> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/responses-acceptance.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), 70);
    let registry = registry();
    for case in cases {
        let contract = registry.resolve(case["type"].as_str().unwrap()).unwrap();
        let decoded = decode_json_for_contract(
            case["json"].as_str().unwrap().as_bytes(),
            Limits::default(),
            &contract,
        );
        let accepted = decoded
            .as_ref()
            .is_ok_and(|data| contract.issues(data).is_empty());
        assert_eq!(
            accepted,
            case["accepted"].as_bool().unwrap(),
            "{case}: {decoded:?}"
        );
        if accepted {
            assert_eq!(fingerprint(&decoded.unwrap()), case["data"], "{case}");
        }
    }
}
#[test]
fn unknown_json_preserves_null_fields_and_does_not_guess_encoded_types() {
    let value = decode_json_preserving(
        br#"{"bytes":"YQ==","date":"2024-02-29T12:00:00Z","duration":"P2D","empty":null}"#,
        Limits::default(),
    )
    .unwrap();
    let Data::Record(fields) = value else {
        panic!("record")
    };
    assert_eq!(fields.len(), 4);
    assert_eq!(fields["empty"], Data::Option(None));
    for name in ["bytes", "date", "duration"] {
        assert!(matches!(&fields[name], Data::Text(_)));
    }
}
#[test]
fn malformed_duplicate_deep_and_excessive_json_is_rejected() {
    for source in [
        "not JSON",
        "{\"id\":1,\"id\":2}",
        "{\"nested\":{\"x\":null,\"x\":2}}",
        "NaN",
        "1e10001",
    ] {
        assert!(
            decode_json_preserving(source.as_bytes(), Limits::default()).is_err(),
            "{source}"
        );
    }
    let deep = format!("{}0{}", "[".repeat(201), "]".repeat(201));
    assert!(decode_json_preserving(deep.as_bytes(), Limits::default()).is_err());
    assert!(decode_json_preserving(b"true", Limits { bytes: 3, nodes: 1 }).is_err());
    assert!(decode_json_preserving(b"[1,2]", Limits { bytes: 5, nodes: 2 }).is_err());
    assert!(decode_json_preserving(b"[1,2]", Limits { bytes: 5, nodes: 3 }).is_ok());
}
#[test]
fn numeric_lexemes_keep_precision_and_contract_decimals_accept_integer_tokens() {
    let types = registry();
    let decimal = types.resolve("Decimal").unwrap();
    let big = b"9007199254740993.000000000000000001";
    let data = decode_json_for_contract(big, Limits::default(), &decimal).unwrap();
    assert_eq!(
        data,
        Data::Decimal("9007199254740993.000000000000000001".parse().unwrap())
    );
    assert!(decimal.issues(&data).is_empty());
    let integer_token = decode_json_for_contract(b"7", Limits::default(), &decimal).unwrap();
    assert_eq!(integer_token, Data::Decimal("7".parse().unwrap()));
    let integer = types.resolve("Int").unwrap();
    assert!(
        !integer
            .issues(&decode_json_for_contract(b"1e3", Limits::default(), &integer).unwrap())
            .is_empty()
    );
}
#[test]
fn response_contract_issues_preserve_escaped_paths_without_values() {
    let types = registry();
    let contract = types.resolve("Escaped").unwrap();
    let data =
        decode_json_for_contract(br#"{"a/b~":"private-value"}"#, Limits::default(), &contract)
            .unwrap();
    let issues = contract.issues(&data);
    assert_eq!(issues[0].path, "/a~1b~0");
    assert!(!format!("{issues:?}").contains("private-value"));
}
