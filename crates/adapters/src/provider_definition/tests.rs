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
