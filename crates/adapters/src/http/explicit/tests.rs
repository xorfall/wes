use super::*;
use crate::codec::{CodecError, decode_json_preserving};
use wes_core::contracts::ContractRegistry;

#[test]
fn unknown_json_response_decodes_without_claiming_a_shape_or_accepting_non_json() {
    let registry = ContractRegistry::new();
    let response = Response {
        contract: Some(registry.resolve("Unknown").unwrap()),
    };
    assert_eq!(response.shape(), Shape::Unknown);
    for body in [
        r#"{"arbitrary":[1,true,null]}"#,
        "42",
        "null",
        "[]",
        r#""text""#,
    ] {
        assert!(
            response.decode(body.as_bytes(), Limits::default()).is_ok(),
            "{body}"
        );
    }
    for body in ["", "not JSON", r#"{"duplicate":1,"duplicate":2}"#] {
        assert!(response.decode(body.as_bytes(), Limits::default()).is_err());
    }
}

#[test]
fn map_response_decode_isolates_contract_validation_from_the_shape_guard() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Envelope: {base: Record, fields: {rates: 'Map<Text, Decimal>'}}}")
        .unwrap();
    for (kind, body) in [
        ("Map<Text, Text>", r#"{"USD":"one"}"#),
        ("Map<Text, Decimal>", "{}"),
        ("Map<Text, Decimal>", r#"{"USD":1.0,"TRY":32.50}"#),
        ("Map<Text, Decimal>", r#"{"USD":1,"TRY":32.50}"#),
        ("Envelope", r#"{"rates":{"USD":1,"TRY":32.50}}"#),
    ] {
        let contract = registry.resolve(kind).unwrap();
        let raw = crate::codec::decode_json_preserving(body.as_bytes(), Limits::default()).unwrap();
        // This guard already accepts populated Maps. Pin that fact independently of decoding.
        assert!(Value::new(contract.shape(), raw.clone(), Provenance::default()).is_ok());
        let issues = contract.issues(&raw);
        let response = Response {
            contract: Some(contract),
        };
        assert!(
            response.decode(body.as_bytes(), Limits::default()).is_ok(),
            "{kind} {body}: shape passed; raw contract issues: {issues:?}"
        );
    }
}

#[test]
fn response_decimal_tokens_follow_nested_contracts_without_native_widening() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n Currency: {base: Text, enum: [USD, TRY]}\n Rate: {base: Decimal, min: 0}\n Rates: {base: 'Map<Currency, Option<Rate>>'}\n Envelope: {base: Record, fields: {rates: {type: Rates, optional: true}}}\n").unwrap();
    let contract = registry.resolve("Option<List<Envelope>>").unwrap();
    let response = Response {
        contract: Some(contract),
    };
    let decoded = response
        .decode(
            br#"[{"rates":{"USD":1,"TRY":32.50}},{"rates":{"TRY":null}},{}]"#,
            Limits::default(),
        )
        .map_err(Failure::error)
        .unwrap();
    assert!(
        response
            .contract
            .as_ref()
            .unwrap()
            .issues(decoded.data())
            .is_empty()
    );

    let contract = registry.resolve("Map<Text, Decimal>").unwrap();
    let raw = decode_json_preserving(br#"{"USD":1}"#, Limits::default()).unwrap();
    assert_eq!(contract.issues(&raw)[0].path, "/USD");
    let response = Response {
        contract: Some(contract),
    };
    let decoded = response
        .decode(
            br#"{"USD":1,"TRY":12345678901234567890.12345678901234567890}"#,
            Limits::default(),
        )
        .map_err(Failure::error)
        .unwrap();
    let Data::Record(values) = decoded.data() else {
        panic!("map result")
    };
    assert_eq!(values["USD"], Data::Decimal("1".parse().unwrap()));
    assert_eq!(
        values["TRY"],
        Data::Decimal("12345678901234567890.12345678901234567890".parse().unwrap())
    );
}

#[test]
fn response_maps_still_enforce_keys_values_nullability_and_parser_budgets() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Currency: {base: Text, enum: [USD, TRY]}, Rate: {base: Decimal, min: 0}}")
        .unwrap();
    for (kind, body) in [
        ("Map<Currency, Rate>", r#"{"OTHER":1}"#),
        ("Map<Currency, Rate>", r#"{"USD":-1}"#),
        ("Map<Text, Decimal>", r#"{"USD":"1.0"}"#),
        ("Map<Text, Decimal>", r#"{"USD":true}"#),
        ("Map<Text, Decimal>", r#"{"USD":null}"#),
        ("Map<Text, Decimal>", "[]"),
        ("Map<Text, Decimal>", r#"{"USD":1,"USD":2}"#),
        ("Map<Text, Decimal>", r#"{"USD":1e10001}"#),
        ("Map<Text, Int>", r#"{"USD":1.5}"#),
        ("Map<Text, Int>", r#"{"USD":1.0}"#),
        ("Map<Text, Text>", r#"{"USD":1}"#),
    ] {
        let response = Response {
            contract: Some(registry.resolve(kind).unwrap()),
        };
        assert!(
            response.decode(body.as_bytes(), Limits::default()).is_err(),
            "{kind} {body}"
        );
    }
    let response = Response {
        contract: Some(registry.resolve("Map<Text, Decimal>").unwrap()),
    };
    for limits in [
        Limits {
            bytes: 3,
            nodes: 100,
        },
        Limits {
            bytes: 100,
            nodes: 1,
        },
    ] {
        assert!(response.decode(br#"{"USD":1}"#, limits).is_err());
    }
}

#[test]
fn union_json_interpretation_matches_example_contract_and_preserves_nullability() {
    let mut registry = ContractRegistry::new();
    registry
        .load(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/union-contracts/types.yaml"
        )))
        .unwrap();
    let response = Response {
        contract: Some(registry.resolve("Content").unwrap()),
    };
    for body in [
        r#""hello""#,
        r#"[{"kind":"count","count":2}]"#,
        r#"[{"kind":"text","text":"hello","note":null}]"#,
        r#"[{"kind":"text","text":"hello","note":"present"}]"#,
    ] {
        assert!(
            response.decode(body.as_bytes(), Limits::default()).is_ok(),
            "{body}"
        );
    }
    for body in [
        r#"[{"kind":"count","count":0}]"#,
        r#"[{"kind":"text","count":2}]"#,
        r#"[{"kind":"text","kind":"count","count":2}]"#,
        "false",
    ] {
        assert!(
            response.decode(body.as_bytes(), Limits::default()).is_err(),
            "{body}"
        );
    }
    let value = response
        .decode(
            br#"[{"kind":"text","text":"hello","note":"present"}]"#,
            Limits::default(),
        )
        .unwrap_or_else(|_| panic!("valid union response rejected"));
    let Data::List(items) = value.data() else {
        panic!()
    };
    let Data::Record(fields) = &items[0] else {
        panic!()
    };
    assert_eq!(
        fields["note"],
        Data::Option(Some(Box::new(Data::Text("present".into()))))
    );
}

#[test]
fn union_foreign_ambiguity_is_order_independent_and_work_is_shared() {
    use crate::codec::CodecError;
    let registry = ContractRegistry::new();
    for name in [
        "Union<Int, Decimal>",
        "Union<Decimal, Int>",
        "Union<Option<Text>, Text>",
        "Union<Text, Option<Text>>",
    ] {
        let raw = if name.contains("Int") {
            "1"
        } else {
            r#""value""#
        };
        assert!(
            matches!(
                decode_json_for_contract(
                    raw.as_bytes(),
                    Limits::default(),
                    &registry.resolve(name).unwrap()
                ),
                Err(CodecError::AmbiguousContract)
            ),
            "{name}"
        );
    }
    assert_eq!(
        decode_json_for_contract(
            b"1",
            Limits::default(),
            &registry.resolve("Union<Int, Int>").unwrap()
        )
        .unwrap(),
        Data::Int(1)
    );
    assert!(matches!(
        decode_json_for_contract(
            br#"["a","b","c"]"#,
            Limits {
                bytes: 1000,
                nodes: 5
            },
            &registry.resolve("Union<List<Int>, List<Text>>").unwrap()
        ),
        Err(CodecError::Work)
    ));
}

#[test]
fn union_typed_reads_use_the_same_json_boundary_as_http() {
    use wes_engine::{calc::LocalServices, driver::CancellationToken};
    let mut registry = ContractRegistry::new();
    registry.load("types: {Named: {base: Record, fields: {note: 'Option<Text>'}}, Choice: {base: 'Union<Int, Named>'}}").unwrap();
    let contract = registry.resolve("Choice").unwrap();
    let body = r#"{"note":"present"}"#;
    let read = crate::codec::CalculationServices
        .read(
            body,
            Some(&contract),
            &CancellationToken::new(),
            wes_language::Span::at(0),
        )
        .unwrap();
    let http = Response {
        contract: Some(contract),
    }
    .decode(body.as_bytes(), Limits::default())
    .unwrap_or_else(|_| panic!("valid union response rejected"));
    assert_eq!(read.data(), http.data());
    assert!(
        crate::codec::CalculationServices
            .read(
                "false",
                Some(&registry.resolve("Choice").unwrap()),
                &CancellationToken::new(),
                wes_language::Span::at(0)
            )
            .is_err()
    );
}

#[test]
fn request_codec_diagnostics_distinguish_limits_and_hide_raw_errors() {
    let registry = ContractRegistry::new();
    let arg = Argument {
        name: "payload".into(),
        wire: "payload".into(),
        location: "body".into(),
        encoding: "json".into(),
        required: true,
        contract: registry.resolve("Unknown").unwrap(),
    };
    for (error, code) in [
        (CodecError::Bytes, "HTTP_REQUEST_BYTES"),
        (CodecError::Work, "HTTP_REQUEST_WORK"),
        (CodecError::Depth, "HTTP_REQUEST_DEPTH"),
        (CodecError::Contract, "HTTP_REQUEST_CONTRACT"),
        (CodecError::AmbiguousContract, "HTTP_REQUEST_AMBIGUOUS"),
        (
            CodecError::Invalid("DO_NOT_ECHO".into()),
            "HTTP_REQUEST_ENCODING",
        ),
    ] {
        let InvocationError::Failed(error) = arg.codec_issue(error).error() else {
            panic!()
        };
        assert_eq!(error.issues()[0].path, "/arguments/payload");
        assert_eq!(error.issues()[0].code, code);
        assert!(!format!("{error:?}").contains("DO_NOT_ECHO"));
    }
    let issues = (0..100)
        .map(|i| wes_core::ValidationIssue {
            path: format!("/arguments/body/{i}"),
            code: "TYP005".into(),
            message: "contract metadata ".repeat(100),
        })
        .collect();
    let InvocationError::Failed(error) = Failure::RequestIssues(issues).error() else {
        panic!()
    };
    assert_eq!(
        error.issues().last().unwrap().code,
        "HTTP_DIAGNOSTICS_LIMIT"
    );
    assert!(
        error
            .issues()
            .iter()
            .map(|i| i.path.len() + i.code.len() + i.message.len())
            .sum::<usize>()
            < 25 * 1024
    );
}

#[test]
fn contract_issue_count_limit_is_reported_even_when_all_returned_issues_fit() {
    let issues = (0..100)
        .map(|i| wes_core::ValidationIssue {
            path: format!("/arguments/body/{i}"),
            code: "TYP005".into(),
            message: "required field is missing".into(),
        })
        .collect();
    let InvocationError::Failed(error) = Failure::RequestIssues(issues).error() else {
        panic!()
    };
    assert_eq!(error.issues().len(), 101);
    assert_eq!(
        error.issues().last().unwrap().code,
        "HTTP_DIAGNOSTICS_LIMIT"
    );
}
