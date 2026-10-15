use super::*;
use indexmap::IndexMap;
use std::{sync::Mutex, time::Duration};
use wes_core::{Data, Provenance, Shape, Value, capability::Safety};
use wes_engine::{
    driver::CancellationToken,
    imports::{ImportMode, ImportRequest, ImportSnapshot, Importer},
    providers::{Call, InvocationError, InvocationFuture, Invoker},
    runtime::{Effect, ExecutionTraits, Runtime},
};

fn document() -> serde_json::Value {
    serde_json::json!({"version":1,"provider":"tools","types":{
        "Positive":{"base":"Int","min":1},
        "Item":{"base":"Record","fields":{"id":"Int","at":"Instant","note":"Option<Text>"}}
    },"backend":{"kind":"process","bin":"/synthetic/tool"},"operations":[{
        "path":["items","view"],"summary":"Read one item",
        "parameters":[{"name":"id","type":"Positive"},{"name":"labels","type":"List<Text>","required":false},{"name":"verbose","type":"Bool","required":false}],
        "argv":["view",{"parameter":"id"},{"parameter":"labels","encoding":"spread","prefix":["--label"]},{"parameter":"verbose","encoding":"flag","prefix":["--verbose"]}],
        "result":{"format":"json","type":"Item"}
    }]})
}

#[test]
fn byte_input_is_strict_required_unconstrained_and_distinct_from_argv() {
    let doc = serde_json::json!({"version":1,"provider":"input","backend":{"kind":"process","bin":"/synthetic/input"},"operations":[{"path":["inspect"],"parameters":[{"name":"source","type":"Bytes"}],"byte_input":{"parameter":"source","max_bytes":1048576},"argv":[],"result":{"format":"json","type":"Text"}}]});
    let compiled = schema::compile(&doc.to_string(), None).unwrap();
    assert_eq!(
        compiled.operations[0]
            .byte_input
            .as_ref()
            .unwrap()
            .parameter,
        "source"
    );
    for mutation in 0..7 {
        let mut invalid = doc.clone();
        match mutation {
            0 => invalid["operations"][0]["byte_input"]["max_bytes"] = 0.into(),
            1 => invalid["operations"][0]["byte_input"]["parameter"] = "absent".into(),
            2 => invalid["operations"][0]["parameters"][0]["required"] = false.into(),
            3 => invalid["operations"][0]["parameters"][0]["type"] = "Text".into(),
            4 => {
                invalid["operations"][0]["argv"] =
                    serde_json::json!([{"parameter":"source","encoding":"json"}])
            }
            5 => invalid["backend"]["kind"] = "session".into(),
            6 => invalid["operations"][0]["byte_input"]["path"] = "/synthetic/input".into(),
            _ => unreachable!(),
        }
        assert!(
            schema::compile(&invalid.to_string(), None).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn exclusive_resources_require_strict_bounded_required_text_paths_on_streaming_sessions() {
    let valid = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":"/synthetic/adapter"},
        "types":{"Scope":{"base":"Record","fields":{"name":"Text","other":"Text","number":"Int","optional":"Option<Text>"}}},
        "operations":[{"path":["watch"],"parameters":[{"name":"scope","type":"Scope"}],"argv":[],"streaming":true,
            "exclusive":[{"namespace":"synthetic.scope","fields":[["scope","name"],["scope","other"]]}],"result":{"format":"json","type":"Int"}}]});
    let definition = schema::compile(&valid.to_string(), Some("alias")).unwrap();
    assert_eq!(definition.operations[0].exclusive.len(), 1);
    for mutation in 0..18 {
        let mut invalid = valid.clone();
        match mutation {
            0 => invalid["operations"][0]["streaming"] = false.into(),
            1 => invalid["backend"]["kind"] = "process".into(),
            2 => invalid["operations"][0]["exclusive"] = serde_json::json!([]),
            3 => invalid["operations"][0]["exclusive"] = serde_json::Value::Null,
            4 => invalid["operations"][0]["exclusive"][0]["namespace"] = "".into(),
            5 => invalid["operations"][0]["exclusive"][0]["namespace"] = "synthetic/scope".into(),
            6 => invalid["operations"][0]["exclusive"][0]["namespace"] = "x".repeat(129).into(),
            7 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([]),
            8 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([["scope","name"],["scope","name"]]),
            9 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([["absent"]]),
            10 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([["scope","absent"]]),
            11 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([["scope","number"]]),
            12 => invalid["operations"][0]["exclusive"][0]["fields"] = serde_json::json!([["scope","optional"]]),
            13 => invalid["operations"][0]["parameters"][0]["required"] = false.into(),
            14 => invalid["operations"][0]["parameters"][0]["type"] = "Option<Scope>".into(),
            15 => invalid["operations"][0]["exclusive"][0]["target"] = "elsewhere".into(),
            16 => invalid["operations"][0]["exclusive"] = serde_json::json!([{"namespace":"synthetic.scope","fields":[["scope","name"]]},{"namespace":"synthetic.scope","fields":[["scope","other"]]}]),
            17 => invalid["operations"][0]["exclusive"] = serde_json::json!((0..5).map(|n| serde_json::json!({"namespace":format!("key{n}"),"fields":[["scope","name"]]})).collect::<Vec<_>>()),
            _ => unreachable!(),
        }
        assert!(
            schema::compile(&invalid.to_string(), None).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn fixed_arguments_are_typed_bounded_and_absent_from_caller_parameters() {
    let mut doc = document();
    doc["operations"][0]["fixed_arguments"] =
        serde_json::json!({"id":9007199254740993i64,"verbose":false});
    let compiled = schema::compile(&doc.to_string(), None).unwrap();
    let op = &compiled.operations[0];
    assert_eq!(
        op.fixed_arguments["id"].data(),
        &Data::Int(9007199254740993)
    );
    let cap = compiled.description.capabilities().next().unwrap();
    assert!(cap.parameter("id").is_none());
    assert!(cap.parameter("verbose").is_none());
    assert!(cap.parameter("labels").is_some());
    for literal in [
        serde_json::Value::Null,
        serde_json::json!([]),
        serde_json::json!(true),
        serde_json::json!({"missing":1}),
        serde_json::json!({"id":0}),
        serde_json::json!({"id":"1"}),
        serde_json::json!({"labels":[1]}),
        serde_json::json!({"labels":["x".repeat(65536)]}),
        serde_json::json!({"labels":vec![""; 10000]}),
    ] {
        let mut invalid = doc.clone();
        invalid["operations"][0]["fixed_arguments"] = literal;
        assert!(schema::compile(&invalid.to_string(), None).is_err());
    }
    let bytes = serde_json::json!({"version":1,"provider":"input","backend":{"kind":"process","bin":"/synthetic/input"},"operations":[{"path":["inspect"],"parameters":[{"name":"source","type":"Bytes"}],"byte_input":{"parameter":"source","max_bytes":1048576},"fixed_arguments":{"source":"YQ=="},"argv":[],"result":{"format":"json","type":"Text"}}]});
    assert!(schema::compile(&bytes.to_string(), None).is_err());
}

#[test]
fn fixed_literal_decoding_keeps_nullable_temporal_and_binary_contract_context() {
    let mut doc = document();
    doc["operations"][0]["parameters"] = serde_json::json!([
        {"name":"duration","type":"Duration"},{"name":"at","type":"Instant"},
        {"name":"optional","type":"Option<Int>"},{"name":"bytes","type":"Bytes"}]);
    doc["operations"][0]["argv"] = serde_json::json!([
        {"parameter":"duration"},{"parameter":"at"},{"parameter":"optional","encoding":"json"},{"parameter":"bytes","encoding":"json"}]);
    doc["operations"][0]["fixed_arguments"] = serde_json::json!({"duration":"PT2S","at":"2031-01-02T03:04:05Z","optional":17,"bytes":"YQ=="});
    let compiled = schema::compile(&doc.to_string(), None).unwrap();
    let values = &compiled.operations[0].fixed_arguments;
    assert!(matches!(values["duration"].data(), Data::Duration(_)));
    assert!(matches!(values["at"].data(), Data::Instant(_)));
    assert_eq!(
        values["optional"].data(),
        &Data::Option(Some(Box::new(Data::Int(17))))
    );
    assert_eq!(values["bytes"].data(), &Data::Bytes(b"a".as_slice().into()));
    doc["operations"][0]["fixed_arguments"]["bytes"] = "Zh==".into();
    assert!(schema::compile(&doc.to_string(), None).is_err());
}

#[tokio::test]
async fn process_fixed_arguments_join_caller_policy_and_refuse_overrides_before_backend_entry() {
    let mut doc = document();
    doc["operations"][0]["fixed_arguments"] = serde_json::json!({"id":17,"verbose":true});
    doc["operations"][0]["result"] = serde_json::json!({"format":"text"});
    let (product, entered) = product(&doc, Ok(output(0, b"fixed")), Default::default());
    assert_eq!(
        error_code(
            product
                .invoker()
                .invoke(
                    call(&product, vec![("id", Data::Int(99))]),
                    CancellationToken::new()
                )
                .await
        ),
        "PRV001"
    );
    assert!(entered.lock().unwrap().is_empty());
    let mut invocation = call(
        &product,
        vec![("labels", Data::List(vec![Data::Text("tag".into())]))],
    );
    let labels = &invocation.arguments["labels"];
    invocation.arguments["labels"] = labels.with_provenance(
        labels
            .provenance()
            .clone()
            .with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    product
        .invoker()
        .invoke(invocation, CancellationToken::new())
        .await
        .unwrap();
    let entered = entered.lock().unwrap();
    assert_eq!(entered.len(), 1);
    assert_eq!(
        entered[0].arguments["args"].data(),
        &Data::List(
            ["view", "17", "--label", "tag", "--verbose"]
                .into_iter()
                .map(|s| Data::Text(s.into()))
                .collect()
        )
    );
    assert!(
        entered[0].arguments["args"]
            .provenance()
            .policy()
            .is_private()
    );
}

#[test]
fn frozen_import_keeps_configured_literals_when_source_content_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut doc = document();
    doc["backend"]["bin"] = std::env::current_exe().unwrap().to_str().unwrap().into();
    doc["operations"][0]["fixed_arguments"] = serde_json::json!({"id":17});
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let importer = ProviderImporter::new(root.path(), Default::default()).unwrap();
    let request = ImportRequest::new(
        "provider".into(),
        None,
        [(
            "file".into(),
            Value::new(
                Shape::Unknown,
                Data::Text("provider.yaml".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let recipe = importer.capture(&request, 256 * 1024).unwrap();
    doc["operations"][0]["fixed_arguments"]["id"] = 99.into();
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let (definition, _) = importer::reconstruct(recipe.source(), false, None).unwrap();
    assert_eq!(
        definition.operations[0].fixed_arguments["id"].data(),
        &Data::Int(17)
    );
    let new_recipe = importer.capture(&request, 256 * 1024).unwrap();
    let (definition, _) = importer::reconstruct(new_recipe.source(), false, None).unwrap();
    assert_eq!(
        definition.operations[0].fixed_arguments["id"].data(),
        &Data::Int(99)
    );
}

#[test]
fn checked_in_definitions_compile_without_probing_their_executables() {
    for source in [include_str!(
        "../../../../examples/declarative-providers/demo.yaml"
    )] {
        let definition = schema::compile(source, None).unwrap();
        assert!(
            definition
                .description
                .capabilities()
                .all(|c| c.safety == Safety::Unsafe)
        );
    }
}

#[test]
fn read_domains_require_one_finite_bool_probe_and_exact_required_text_scope_on_every_operation() {
    let mut valid = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":"/synthetic/adapter"},"read_access":{"scope":["realm"],"revalidate":["probe"]},"operations":[{"path":["probe"],"parameters":[{"name":"realm","type":"Text"}],"argv":[],"result":{"format":"json","type":"Bool"}},{"path":["read"],"parameters":[{"name":"realm","type":"Text"}],"argv":[],"result":{"format":"json","type":"Text"}}]});
    assert!(schema::compile(&valid.to_string(), None).is_ok());
    for mutation in 0..8 {
        let mut invalid = valid.clone();
        match mutation {
            0 => invalid["read_access"]["scope"] = serde_json::json!([]),
            1 => invalid["read_access"]["scope"] = serde_json::json!(["realm", "realm"]),
            2 => invalid["read_access"]["revalidate"] = serde_json::json!(["absent"]),
            3 => invalid["operations"][0]["streaming"] = true.into(),
            4 => invalid["operations"][0]["result"]["type"] = "Text".into(),
            5 => invalid["operations"][1]["parameters"][0]["required"] = false.into(),
            6 => invalid["operations"][1]["parameters"][0]["type"] = "Option<Text>".into(),
            7 => invalid["operations"][0]["parameters"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"name":"extra","type":"Text"})),
            _ => unreachable!(),
        }
        assert!(
            schema::compile(&invalid.to_string(), None).is_err(),
            "mutation {mutation}"
        );
    }
    valid["read_access"]["permission"] = "invented".into();
    assert!(schema::compile(&valid.to_string(), None).is_err());
}
fn output(code: i64, stdout: &[u8]) -> Value {
    Value::new(
        crate::process::output_shape(),
        Data::Record(IndexMap::from([
            ("exitCode".into(), Data::Int(code)),
            ("stdout".into(), Data::Bytes(stdout.into())),
            (
                "stderr".into(),
                Data::Bytes(b"synthetic failure".as_slice().into()),
            ),
        ])),
        Provenance::default(),
    )
    .unwrap()
}
#[derive(Clone)]
struct Spy {
    calls: Arc<Mutex<Vec<Call>>>,
    result: Result<Value, InvocationError>,
}
impl Invoker for Spy {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.calls.lock().unwrap().push(call);
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}
fn product(
    doc: &serde_json::Value,
    result: Result<Value, InvocationError>,
    config: crate::process::ProcessConfig,
) -> (wes_engine::imports::ImportProduct, Arc<Mutex<Vec<Call>>>) {
    let definition = schema::compile(&doc.to_string(), None).unwrap();
    let (base, _) = crate::process::wrapping("tools", "/synthetic/tool", config).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let spy = Spy {
        calls: calls.clone(),
        result,
    };
    (
        invocation::wrap(definition, &base, Arc::new(spy), config, vec![]).unwrap(),
        calls,
    )
}
fn call(product: &wes_engine::imports::ImportProduct, args: Vec<(&str, Data)>) -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| match e {
            Effect::Spawn(t) => Some(t.run),
            _ => None,
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability: product.description().capabilities().next().unwrap().clone(),
        arguments: args
            .into_iter()
            .map(|(k, d)| {
                (
                    k.into(),
                    Value::new(Shape::Unknown, d, Provenance::default()).unwrap(),
                )
            })
            .collect(),
    }
}
fn error_code(result: Result<Value, InvocationError>) -> String {
    match result {
        Err(InvocationError::Failed(e)) => e.code().into(),
        other => panic!("expected failure: {other:?}"),
    }
}

#[tokio::test]
async fn maps_literal_arguments_and_returns_captured_temporal_nullable_contract() {
    let (product, calls) = product(
        &document(),
        Ok(output(
            0,
            br#"{"id":9007199254740993,"at":"2026-10-08T12:18:19Z","note":null}"#,
        )),
        Default::default(),
    );
    assert_eq!(
        product.description().capabilities().next().unwrap().safety,
        Safety::Unsafe
    );
    let call = call(
        &product,
        vec![
            ("id", Data::Int(4)),
            (
                "labels",
                Data::List(vec![
                    Data::Text("a b".into()),
                    Data::Text("$(never-execute);'\"".into()),
                ]),
            ),
            ("verbose", Data::Bool(true)),
        ],
    );
    let run = call.run.clone();
    let result = product
        .invoker()
        .invoke(call, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.shape(),Shape::Record(r) if r.name()=="Item"));
    assert!(result.metadata().is_some());
    let Data::Record(fields) = result.data() else {
        panic!("record")
    };
    assert_eq!(fields["id"], Data::Int(9007199254740993));
    assert!(matches!(fields["at"], Data::Instant(_)));
    assert!(matches!(fields["note"], Data::Option(None)));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].run, run);
    assert_eq!(calls[0].capability.path, ["run"]);
    assert_eq!(
        calls[0].arguments["args"].data(),
        &Data::List(
            [
                "view",
                "4",
                "--label",
                "a b",
                "--label",
                "$(never-execute);'\"",
                "--verbose"
            ]
            .map(|s| Data::Text(s.into()))
            .to_vec()
        )
    );
}
#[tokio::test]
async fn rejects_argument_contracts_nul_and_count_limits_before_entering_backend() {
    let (product, calls) = product(&document(), Ok(output(0, b"{}")), Default::default());
    for args in [
        vec![],
        vec![("id", Data::Int(0))],
        vec![("id", Data::Int(1)), ("unknown", Data::Bool(true))],
        vec![
            ("id", Data::Int(1)),
            ("labels", Data::List(vec![Data::Text("bad\0value".into())])),
        ],
        vec![
            ("id", Data::Int(1)),
            ("labels", Data::List(vec![Data::Text("x".into()); 1024])),
        ],
    ] {
        assert_eq!(
            error_code(
                product
                    .invoker()
                    .invoke(call(&product, args), CancellationToken::new())
                    .await
            ),
            "PRV001"
        );
    }
    assert!(calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn byte_limits_apply_to_list_expansion_and_repeated_prefixes() {
    let config = crate::process::ProcessConfig {
        argument_bytes: 64,
        ..Default::default()
    };
    let (product, calls) = product(&document(), Ok(output(0, b"{}")), config);
    for labels in [
        vec![Data::Text("x".repeat(65).into())],
        vec![Data::Text("123456".into()); 8],
    ] {
        assert_eq!(
            error_code(
                product
                    .invoker()
                    .invoke(
                        call(
                            &product,
                            vec![("id", Data::Int(1)), ("labels", Data::List(labels))]
                        ),
                        CancellationToken::new()
                    )
                    .await
            ),
            "PRV001"
        );
    }
    assert!(calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn nonzero_exit_is_not_json_and_malformed_or_wrong_json_is_not_success() {
    for (code, body, expected) in [
        (9, b"not json".as_slice(), "PRV003"),
        (0, b"not json", "PRV004"),
        (0, b"{}", "PRV004"),
        (0, br#"{"id":1,"id":2}"#, "PRV004"),
    ] {
        let (product, calls) = product(&document(), Ok(output(code, body)), Default::default());
        assert_eq!(
            error_code(
                product
                    .invoker()
                    .invoke(
                        call(&product, vec![("id", Data::Int(1))]),
                        CancellationToken::new()
                    )
                    .await
            ),
            expected
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn supports_text_bytes_raw_process_and_explicit_success_codes() {
    for format in ["text", "bytes", "process"] {
        let mut doc = document();
        doc["operations"][0]["result"] = serde_json::json!({"format":format});
        doc["operations"][0]["success_codes"] = serde_json::json!([0, 1]);
        let (product, _) = product(&doc, Ok(output(1, b"hello\n")), Default::default());
        let result = product
            .invoker()
            .invoke(
                call(&product, vec![("id", Data::Int(1))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        match format {
            "text" => assert_eq!(result.data(), &Data::Text("hello\n".into())),
            "bytes" => assert_eq!(result.data(), &Data::Bytes(b"hello\n".as_slice().into())),
            _ => assert_eq!(result.shape(), &crate::process::output_shape()),
        }
    }
}

fn record_bytes_document() -> serde_json::Value {
    serde_json::json!({"version":1,"provider":"blob","backend":{"kind":"process","bin":"/synthetic/tool"},"types":{
        "BinaryPart":{"base":"Record","fields":{"raw":"Bytes"}},
        "Blob":{"base":"Record","fields":{"label":"Text","nested":"BinaryPart"}}
    },"operations":[{"path":["read"],"argv":[],"result":{"format":"record-bytes","type":"Blob","byte_field":["nested","raw"]}}]})
}
#[test]
fn record_bytes_requires_a_static_required_unconstrained_binary_leaf() {
    let valid = record_bytes_document();
    assert!(schema::compile(&valid.to_string(), None).is_ok());
    for mutation in 0..8 {
        let mut doc = valid.clone();
        match mutation {
            0 => doc["operations"][0]["result"]["byte_field"] = serde_json::json!([]),
            1 => doc["operations"][0]["result"]["byte_field"] = serde_json::json!(["missing"]),
            2 => doc["types"]["BinaryPart"]["fields"]["raw"] = "Text".into(),
            3 => {
                doc["types"]["BinaryPart"]["fields"]["raw"] =
                    serde_json::json!({"type":"Bytes","optional":true})
            }
            4 => {
                doc["types"]["BinaryPart"]["fields"]["raw"] =
                    serde_json::json!({"base":"Bytes","enum":[""]})
            }
            5 => doc["operations"][0]["result"]["format"] = "json".into(),
            6 => doc["operations"][0]["result"]["type"] = "Bytes".into(),
            7 => doc["backend"]["kind"] = "session".into(),
            _ => unreachable!(),
        }
        assert!(
            schema::compile(&doc.to_string(), None).is_err(),
            "mutation {mutation}"
        );
    }
}
#[tokio::test]
async fn record_bytes_materialized_result_preserves_exact_payload_and_refuses_bad_headers() {
    let header = br#"{"label":"synthetic","nested":{"raw":""}}"#;
    let mut stdout = (header.len() as u32).to_be_bytes().to_vec();
    stdout.extend_from_slice(header);
    stdout.extend_from_slice(&[0, 255, 13, 10]);
    let (p, calls) = product(
        &record_bytes_document(),
        Ok(output(0, &stdout)),
        Default::default(),
    );
    let value = p
        .invoker()
        .invoke(call(&p, vec![]), CancellationToken::new())
        .await
        .unwrap();
    assert!(value.metadata().is_some());
    let Data::Record(fields) = value.data() else {
        panic!("record");
    };
    let Data::Record(nested) = &fields["nested"] else {
        panic!("nested");
    };
    assert_eq!(nested["raw"], Data::Bytes(vec![0, 255, 13, 10].into()));
    assert_eq!(calls.lock().unwrap().len(), 1);
    for (code, bytes, expected) in [
        (0, vec![0, 0, 0, 0], "PRV004"),
        (0, stdout[..8].to_vec(), "PRV004"),
        (7, stdout.clone(), "PRV003"),
        (0, vec![255; 4], "PRV004"),
    ] {
        let (p, calls) = product(
            &record_bytes_document(),
            Ok(output(code, &bytes)),
            Default::default(),
        );
        assert_eq!(
            error_code(
                p.invoker()
                    .invoke(call(&p, vec![]), CancellationToken::new())
                    .await
            ),
            expected
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}
#[test]
fn record_bytes_decoder_accepts_every_header_boundary_without_storing_framing_bytes() {
    let definition = schema::compile(&record_bytes_document().to_string(), None).unwrap();
    let contract = definition.operations[0].result.as_ref().unwrap();
    let fields = definition.operations[0].byte_field.as_ref().unwrap();
    let header = br#"{"label":"synthetic","nested":{"raw":""}}"#;
    let mut input = (header.len() as u32).to_be_bytes().to_vec();
    input.extend_from_slice(header);
    input.extend_from_slice(&[255, 0, 7]);
    for split in 0..=input.len() {
        let token = CancellationToken::new();
        let mut decoder = super::record_bytes::Decoder::default();
        let mut raw = decoder
            .push(&input[..split], contract, fields, &token)
            .unwrap();
        raw.extend(
            decoder
                .push(&input[split..], contract, fields, &token)
                .unwrap(),
        );
        decoder.finish(Provenance::default(), &token).unwrap();
        assert_eq!(raw, [255, 0, 7], "split {split}");
    }
}
#[tokio::test]
async fn optional_false_and_empty_list_omit_prefixes_and_json_encoding_is_explicit() {
    let (p, calls) = product(
        &document(),
        Ok(output(
            0,
            br#"{"id":1,"at":"2026-10-08T12:18:19Z","note":null}"#,
        )),
        Default::default(),
    );
    p.invoker()
        .invoke(
            call(
                &p,
                vec![
                    ("id", Data::Int(1)),
                    ("verbose", Data::Bool(false)),
                    ("labels", Data::List(vec![])),
                ],
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap()[0].arguments["args"].data(),
        &Data::List(vec![Data::Text("view".into()), Data::Text("1".into())])
    );
    let mut doc = document();
    doc["operations"][0]["parameters"] = serde_json::json!([{"name":"payload","type":"List<Int>"}]);
    doc["operations"][0]["argv"] = serde_json::json!([{"parameter":"payload","encoding":"json"}]);
    doc["operations"][0]["result"] = serde_json::json!({"format":"text"});
    let (p, calls) = product(&doc, Ok(output(0, b"ok")), Default::default());
    p.invoker()
        .invoke(
            call(
                &p,
                vec![("payload", Data::List(vec![Data::Int(1), Data::Int(2)]))],
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap()[0].arguments["args"].data(),
        &Data::List(vec![Data::Text("[1,2]".into())])
    );
}
#[tokio::test]
async fn retains_private_argument_policy_and_propagates_unknown_outcomes_without_retry() {
    let result = Err(InvocationError::Failed(
        wes_core::ErrorValue::new(
            wes_core::ErrorId::new("unknown").unwrap(),
            wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
            "unknown",
            vec![],
            None,
        )
        .unwrap(),
    ));
    let (p, calls) = product(&document(), result, Default::default());
    let mut call = call(
        &p,
        vec![("id", Data::Int(1)), ("verbose", Data::Bool(false))],
    );
    let value = call.arguments["verbose"].clone();
    call.arguments.insert(
        "verbose".into(),
        value.with_provenance(
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        ),
    );
    assert_eq!(
        error_code(p.invoker().invoke(call, CancellationToken::new()).await),
        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].arguments["args"]
            .provenance()
            .policy()
            .is_private()
    );
}
#[tokio::test]
async fn cancellation_before_dispatch_never_enters_backend() {
    let (p, calls) = product(&document(), Ok(output(0, b"{}")), Default::default());
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        p.invoker()
            .invoke(call(&p, vec![("id", Data::Int(1))]), token)
            .await,
        Err(InvocationError::Cancelled)
    ));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_json_none_is_null_and_scalar_none_omits_its_prefix() {
    for (encoding, expected) in [
        (
            "json",
            vec![Data::Text("--value".into()), Data::Text("null".into())],
        ),
        ("scalar", vec![]),
    ] {
        let mut doc = document();
        doc["operations"][0]["parameters"] =
            serde_json::json!([{"name":"value","type":"Option<Text>"}]);
        doc["operations"][0]["argv"] =
            serde_json::json!([{"parameter":"value","encoding":encoding,"prefix":["--value"]}]);
        doc["operations"][0]["result"] = serde_json::json!({"format":"text"});
        let (p, calls) = product(&doc, Ok(output(0, b"ok")), Default::default());
        p.invoker()
            .invoke(
                call(&p, vec![("value", Data::Option(None))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            calls.lock().unwrap()[0].arguments["args"].data(),
            &Data::List(expected)
        );
    }
}

#[tokio::test]
async fn private_process_failure_does_not_export_stderr_details() {
    let private = output(7, b"not decoded").with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    let (p, calls) = product(&document(), Ok(private), Default::default());
    let Err(InvocationError::Failed(error)) = p
        .invoker()
        .invoke(
            call(&p, vec![("id", Data::Int(1))]),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("failure expected")
    };
    assert!(error.policy().is_private());
    assert!(error.issues().is_empty());
    assert!(!error.message().contains("synthetic failure"));
    assert_eq!(calls.lock().unwrap().len(), 1);
}
#[test]
fn strict_definition_validation_rejects_unknown_fields_paths_types_and_encodings() {
    let mut cases = vec![];
    let mut d = document();
    d["extra"] = true.into();
    cases.push(d);
    let mut d = document();
    d["backend"]["shell"] = "forbidden".into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["safety"] = "safe".into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["argv"][1]["typo"] = true.into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["argv"][1]["parameter"] = "missing".into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["argv"][1]["encoding"] = "flag".into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["result"]["type"] = "Iter<Text>".into();
    cases.push(d);
    let mut d = document();
    d["operations"][0]["parameters"][0]["name"] = "with space".into();
    cases.push(d);
    let mut d = document();
    let op = d["operations"][0].clone();
    d["operations"].as_array_mut().unwrap().push(op);
    cases.push(d);
    let mut d = document();
    d["operations"][0]["success_codes"] = serde_json::json!([]);
    cases.push(d);
    for d in cases {
        assert!(schema::compile(&d.to_string(), None).is_err(), "{d}");
    }
    for source in [
        "version: 1\nversion: 1",
        "provider: &anchor tools",
        "version: !!int 1",
        "version: 1\n---\nversion: 1",
    ] {
        assert!(schema::compile(source, None).is_err());
    }
}
#[test]
fn import_rebuild_uses_frozen_definition_and_never_rereads_files() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut doc = document();
    doc["backend"]["bin"] = executable.to_str().unwrap().into();
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let importer = ProviderImporter::new(root.path(), Default::default()).unwrap();
    let request = ImportRequest::new(
        "provider".into(),
        Some("custom".into()),
        [(
            "file".into(),
            Value::new(
                Shape::Unknown,
                Data::Text("provider.yaml".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let recipe = importer.capture(&request, 256 * 1024).unwrap();
    std::fs::remove_file(root.path().join("provider.yaml")).unwrap();
    let snapshot = ImportSnapshot::new(request, recipe);
    let replay = ProviderImporter::new(elsewhere.path(), Default::default())
        .unwrap()
        .build(&snapshot, ImportMode::Replay)
        .unwrap();
    assert_eq!(replay.description().name(), "custom");
    assert_eq!(
        replay.description().capabilities().next().unwrap().path,
        ["items", "view"]
    );
    assert!(
        std::fs::read_dir(elsewhere.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn target_definitions_capture_host_yaml_and_build_remote_contracts_without_probes() {
    use wes_engine::environments::EnvironmentLoader;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("key"), b"synthetic key").unwrap();
    std::fs::write(root.path().join("hosts"), b"synthetic hosts").unwrap();
    let mut doc = document();
    doc["backend"]["bin"] = "/not-installed-on-host/tool".into();
    std::fs::write(root.path().join("tool.yaml"), doc.to_string()).unwrap();
    let loader = crate::environments::LocalEnvironments::new(root.path()).unwrap();
    let targets = [
        serde_json::json!({"kind":"docker","socket":"/not-listening.sock","container":"synthetic","inherit":"container"}),
        serde_json::json!({"kind":"ssh","client":"/not-installed/ssh","host":"fixture.invalid","user":"qa","identity_file":root.path().join("key"),"known_hosts":root.path().join("hosts"),"shell":"posix","inherit":"remote"}),
    ];
    for target in targets {
        let mut env = serde_json::json!({"version":1,"targets":{"remote":target},"environments":{"demo":{"imports":{"tools":{"source":{"kind":"provider","file":"tool.yaml"},"bind":{"target":"remote"}}}}}});
        std::fs::write(root.path().join("env.yaml"), env.to_string()).unwrap();
        let captured = loader.capture("env.yaml").unwrap();
        let package = wes_core::environments::Package::parse(&captured.yaml).unwrap();
        let effective = wes_core::environments::EffectiveEnvironment::resolve(
            &package,
            "demo",
            None,
            &captured.sources,
        )
        .unwrap();
        let binding = Arc::new(effective).bind("tools").unwrap();
        let product = loader.build("tools", &binding).unwrap();
        assert_eq!(
            product.description().capabilities().next().unwrap().path,
            ["items", "view"]
        );
        // The same YAML cannot silently switch its executable namespace between targets.
        env["targets"]["host"] = serde_json::json!({"kind":"local"});
        env["environments"]["host"] = serde_json::json!({"imports":{"tools":{"source":{"kind":"provider","file":"tool.yaml"},"bind":{"target":"host"}}}});
        std::fs::write(root.path().join("env.yaml"), env.to_string()).unwrap();
        assert!(loader.capture("env.yaml").is_err());
    }
}

#[test]
fn control_output_policies_are_static_strict_and_default_to_public() {
    let doc = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":"/synthetic/adapter"},"operations":[{"path":["watch"],"argv":[],"streaming":true,"result":{"format":"json","type":"Option<Text>"},"control":{"input":"Int","output":"Bool"}}]});
    let default = schema::compile(&doc.to_string(), None).unwrap();
    assert_eq!(
        default.operations[0].control.as_ref().unwrap().policy,
        wes_core::flow::OutputPolicy::Public
    );
    for (text, expected) in [
        ("public", wes_core::flow::OutputPolicy::Public),
        ("private", wes_core::flow::OutputPolicy::Private),
        (
            "confidential-temporary",
            wes_core::flow::OutputPolicy::ConfidentialTemporary,
        ),
        ("confidential", wes_core::flow::OutputPolicy::Confidential),
    ] {
        let mut declared = doc.clone();
        declared["operations"][0]["control"]["output_policy"] = text.into();
        let compiled = schema::compile(&declared.to_string(), None).unwrap();
        assert_eq!(
            compiled.operations[0].control.as_ref().unwrap().policy,
            expected
        );
    }
    for invalid in [
        serde_json::json!("secret"),
        serde_json::json!(true),
        serde_json::Value::Null,
        serde_json::json!({"policy":"private"}),
    ] {
        let mut invalid_doc = doc.clone();
        invalid_doc["operations"][0]["control"]["output_policy"] = invalid;
        assert!(schema::compile(&invalid_doc.to_string(), None).is_err());
    }
}

#[test]
fn provider_structure_diagnostics_publish_choices_without_submitted_values() {
    for (pointer, value, expected) in [
        (
            "/private-field",
            serde_json::json!("private-value"),
            "valid fields: version, provider, types, backend, operations, read_access",
        ),
        (
            "/backend/private-field",
            serde_json::json!("private-value"),
            "valid fields: kind, bin",
        ),
        (
            "/backend/kind",
            serde_json::json!("private-value"),
            "valid kind values: process, session",
        ),
        (
            "/operations/0/private-field",
            serde_json::json!("private-value"),
            "valid fields: path, summary, parameters, argv, result, success_codes, streaming, control",
        ),
        (
            "/operations/0/result/private-field",
            serde_json::json!("private-value"),
            "valid fields: format, type, byte_field",
        ),
        (
            "/operations/0/result/format",
            serde_json::json!("private-value"),
            "valid values: json, text, bytes, process",
        ),
        (
            "/operations/0/parameters/0/private-field",
            serde_json::json!("private-value"),
            "valid fields: name, type, required",
        ),
        (
            "/operations/0/control",
            serde_json::json!({"input":"Text","output":"Bool","private-field":"private-value"}),
            "valid fields: input, output, output_policy",
        ),
        (
            "/read_access",
            serde_json::json!({"scope":["id"],"revalidate":["probe"],"private-field":"private-value"}),
            "valid fields: scope, revalidate",
        ),
    ] {
        let mut doc = document();
        // Insert only at a known structural location, never interpolate source into errors.
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        let parent = doc.pointer_mut(parent).unwrap().as_object_mut().unwrap();
        parent.insert(field.into(), value);
        let error = match schema::compile(&doc.to_string(), None) {
            Err(error) => error,
            Ok(_) => panic!("invalid structure accepted"),
        };
        let error: wes_engine::imports::ImportError = error.into();
        let message = error.to_string();
        assert!(message.contains(expected), "{message}");
        assert!(!message.contains("private-field"), "{message}");
        assert!(!message.contains("private-value"), "{message}");
        assert_eq!(error.diagnostic(wes_language::Span::at(0)).code, "IMP001");
    }
    let mut semantic = document();
    semantic["operations"][0]["result"]["type"] = "private-value".into();
    let node = wes_core::contracts::read_strict_package(&semantic.to_string()).unwrap();
    wes_core::package_schema::declarations()
        .validate("provider.document", &node)
        .unwrap();
    assert!(
        schema::compile(&semantic.to_string(), None).is_err(),
        "structural validity must not bypass resolved contract checks"
    );
}

#[test]
fn streaming_recording_declarations_refuse_before_dispatch() {
    let valid = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":"synthetic-peer"},
        "types":{"Blob":{"base":"Record","fields":{"label":"Text","raw":"Bytes"}}},
        "operations":[{"path":["watch"],"streaming":true,"argv":[],"result":{"format":"json","type":"Int"},
            "recording":{"type":"Blob","byte_field":["raw"],"max_bytes":65536,"max_duration_ms":1000}}]});
    assert!(schema::compile(&valid.to_string(), None).is_ok());
    for (key, value) in [
        ("max_bytes", serde_json::json!(0)),
        (
            "max_bytes",
            serde_json::json!(wes_budgets::get("storage.acquisition.bytes") + 1),
        ),
        ("max_duration_ms", serde_json::json!(0)),
        ("max_duration_ms", serde_json::json!(86_400_001)),
        ("byte_field", serde_json::json!([])),
        ("byte_field", serde_json::json!(["label"])),
        ("type", serde_json::json!("Missing")),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut document = valid.clone();
        document["operations"][0]["recording"][key] = value;
        assert!(
            schema::compile(&document.to_string(), None).is_err(),
            "{key}"
        );
    }
    let mut finite = valid.clone();
    finite["operations"][0]["streaming"] = serde_json::json!(false);
    assert!(schema::compile(&finite.to_string(), None).is_err());
    let mut process = valid.clone();
    process["backend"]["kind"] = serde_json::json!("process");
    assert!(schema::compile(&process.to_string(), None).is_err());
}
