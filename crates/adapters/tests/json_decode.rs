use std::sync::Arc;
use wes_adapters::codec::{self, CalculationServices};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value, capability::Catalogue, contracts::ContractRegistry,
    flow::FlowPolicy,
};
use wes_engine::{
    calc::{LocalServices, Machine, Step},
    driver::CancellationToken,
};
use wes_language::{
    Span,
    calc::{self, Package},
};

fn fields(value: &Value) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("decode result")
    };
    fields
}
fn error_code(value: &Value) -> &str {
    let Data::Option(Some(error)) = &fields(value)["error"] else {
        panic!("diagnostic")
    };
    let Data::Record(error) = error.as_ref() else {
        panic!("diagnostic record")
    };
    let Data::Text(code) = &error["code"] else {
        panic!("diagnostic code")
    };
    code
}

#[test]
fn decode_distinguishes_successful_null_exact_scalars_and_invalid_bytes() {
    let token = CancellationToken::new();
    for bytes in [
        b"null".as_slice(),
        b"1",
        b"1.0",
        b"true",
        b"[1,null]",
        b"{\"x\":1}",
    ] {
        let value = CalculationServices
            .decode(bytes, None, &token, Span::at(0))
            .unwrap();
        assert_eq!(fields(&value)["ok"], Data::Bool(true));
        assert_eq!(fields(&value)["error"], Data::Option(None));
        let Data::Option(Some(decoded)) = &fields(&value)["value"] else {
            panic!("successful value")
        };
        assert_eq!(
            decoded.as_ref(),
            &codec::decode_json_preserving(bytes, codec::Limits::default()).unwrap()
        );
    }
    for bytes in [
        b"{\"secret-sentinel\":}".as_slice(),
        b"",
        b"\xff",
        b"{\"x\":1,\"x\":2}",
    ] {
        let value = CalculationServices
            .decode(bytes, None, &token, Span::at(0))
            .unwrap();
        assert_eq!(fields(&value)["ok"], Data::Bool(false));
        assert_eq!(fields(&value)["value"], Data::Option(None));
        assert_eq!(error_code(&value), "JSON001");
        assert!(!format!("{value:?}").contains("secret-sentinel"));
    }
    assert!(
        CalculationServices
            .read("{", None, &token, Span::at(0))
            .is_err()
    );
}

#[test]
fn decode_checks_native_contracts_without_echoing_mismatch_or_ambiguity() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Row: {base: Record, fields: {amount: Int}}}")
        .unwrap();
    let token = CancellationToken::new();
    let row = registry.resolve("Row").unwrap();
    for json in [
        b"null".as_slice(),
        b"{\"amount\":\"secret-sentinel\"}",
        b"{}",
    ] {
        let value = CalculationServices
            .decode(json, Some(&row), &token, Span::at(0))
            .unwrap();
        assert_eq!(error_code(&value), "JSON002");
        assert_eq!(value.shape(), &calc::json_decode_shape(row.shape()));
        assert!(!format!("{value:?}").contains("secret-sentinel"));
    }
    let union = registry.resolve("Union<Int,Decimal>").unwrap();
    let value = CalculationServices
        .decode(b"1", Some(&union), &token, Span::at(0))
        .unwrap();
    assert_eq!(error_code(&value), "JSON003");
}

#[test]
fn decoder_never_turns_resource_exhaustion_or_cancellation_into_data() {
    let token = CancellationToken::new();
    for json in [
        format!("\"{}\"", "x".repeat(1024 * 1024)),
        format!("[{}0{}]", "[".repeat(201), "]".repeat(201)),
        format!("[{}0]", "0,".repeat(100_001)),
        format!(
            "{{{}}}",
            (0..100_001)
                .map(|i| format!("\"{i}\":0"))
                .collect::<Vec<_>>()
                .join(",")
        ),
    ] {
        let error = CalculationServices
            .decode(json.as_bytes(), None, &token, Span::at(0))
            .unwrap_err();
        assert_eq!(error.code, "CAL006");
    }
    // The common codec exposes collection resource refusal as typed Work, not a JSON diagnostic.
    for json in [
        b"[0,0,0]".as_slice(),
        b"{\"a\":0,\"b\":0,\"c\":0,\"d\":0,\"e\":0,\"f\":0,\"g\":0,\"h\":0,\"i\":0}",
    ] {
        assert!(matches!(
            codec::decode_json_preserving(
                json,
                codec::Limits {
                    bytes: 1024,
                    nodes: 2
                }
            ),
            Err(codec::CodecError::Work)
        ));
    }
    token.cancel();
    assert!(
        CalculationServices
            .decode(b"{", None, &token, Span::at(0))
            .unwrap_err()
            .cancelled
    );
}

#[test]
fn decoded_bytes_keep_captured_contract_and_private_source_policy() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Row: {base: Record, fields: {amount: Int}}}")
        .unwrap();
    let program = Arc::new(
        calc::parse_body("return decodeJson($body,'Row');", 0, Package::standard()).unwrap(),
    );
    let compiled = Arc::new(
        calc::analyze(
            program,
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &registry,
                workspace: &|name| (name == "body").then_some(Shape::Primitive(Primitive::Bytes)),
            },
        )
        .unwrap(),
    );
    // A later registry revision cannot change the already captured decoder contract.
    let mut newer_registry = ContractRegistry::new();
    newer_registry
        .load("types: {Row: {base: Record, fields: {amount: Text}}}")
        .unwrap();
    drop(registry);
    for bytes in [b"{\"amount\":1}".as_slice(), b"{\"amount\":\"x\"}", b"\xff"] {
        let input = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(bytes.into()),
            Provenance::default().with_policy(&FlowPolicy::default().private().from_origin("lab")),
        )
        .unwrap();
        let mut machine = Machine::new(
            compiled.clone(),
            [("body".into(), input)].into_iter().collect(),
            Default::default(),
        )
        .unwrap();
        let token = CancellationToken::new();
        loop {
            match machine.poll(&token).unwrap() {
                Step::Yield => {}
                Step::Request(wes_engine::calc::Request::Json {
                    id,
                    bytes,
                    contract,
                    mode,
                    span,
                }) => {
                    machine
                        .resume(
                            id,
                            CalculationServices.json(
                                &bytes,
                                contract.as_deref(),
                                &token,
                                span,
                                mode,
                            ),
                        )
                        .unwrap();
                }
                Step::Complete(value) => {
                    assert_eq!(fields(&value)["ok"], Data::Bool(bytes == b"{\"amount\":1}"));
                    assert!(value.provenance().policy().is_private());
                    assert!(value.provenance().policy().origins().contains("lab"));
                    break;
                }
                _ => panic!("decoder requested external acquisition"),
            }
        }
    }
}
