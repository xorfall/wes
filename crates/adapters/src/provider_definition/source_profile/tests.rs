use super::*;
use wes_engine::{
    driver::CancellationToken,
    imports::{ImportRequest, Importer},
    providers::Call,
    runtime::{Effect, ExecutionTraits, Runtime},
    streams,
};

fn document() -> Json {
    serde_json::json!({"version":1,"provider":"sampler","backend":{"kind":"session","bin":"/synthetic/never-launch"},"types":{
        "Config":{"base":"Record","fields":{"duration":"Int","packets":"Int"}},
        "Stop":{"base":"Text","enum":["stop"]}
    },"operations":[{"path":["start"],"streaming":true,"argv":[],"result":{"format":"json","type":"Int"},
        "parameters":[{"name":"config","type":"Config"},{"name":"duration_ms","type":"Int","required":false},{"name":"packet_limit","type":"Int","required":false},{"name":"condition","type":"Text","required":false}],
        "fixed_arguments":{"config":{"duration":30000,"packets":1000}},"control":{"input":"Stop","output":"Bool"},
        "source_profile":{"name":"metrics-lab","kind":"metrics","default":true,"stop":"stop","tail_ms":5000,
            "narrowing":{"duration":{"kind":"maximum","parameter":"duration_ms","maximum_from":["config","duration"]},"packets":{"kind":"maximum","parameter":"packet_limit","maximum_from":["config","packets"]},"filter":{"kind":"backend-condition","parameter":"condition"}}}
    }]})
}
fn integer(n: i64) -> Value {
    Value::new(
        wes_core::Shape::Primitive(wes_core::Primitive::Int),
        Data::Int(n),
        Default::default(),
    )
    .unwrap()
}

#[test]
fn profile_metadata_is_typed_static_and_limits_come_from_original_fixed_fields() {
    let definition = super::super::schema::compile(&document().to_string(), None).unwrap();
    let op = &definition.operations[0];
    let profile = op.source_profile.as_ref().unwrap();
    assert_eq!(profile.name(), "metrics-lab");
    assert!(profile.is_default());
    assert!(matches!(
        profile.options()["duration"].restriction,
        Restriction::Maximum(30000)
    ));
    assert!(
        op.bind_arguments(&[("duration_ms".into(), integer(1000))].into())
            .is_ok()
    );
    for value in [0, -1, 30001] {
        assert!(
            op.bind_arguments(&[("duration_ms".into(), integer(value))].into())
                .is_err()
        );
    }
    assert!(
        op.bind_arguments(&[("config".into(), integer(1))].into())
            .is_err()
    );
    let product =
        super::super::session::unbound(definition, crate::process::ProcessConfig::default())
            .unwrap();
    let port = product.streams().unwrap();
    let metadata = port
        .source_profile(product.description().capabilities().next().unwrap())
        .unwrap();
    assert_eq!(metadata.name(), "metrics-lab");
    assert_eq!(metadata.kind(), "metrics");
}

#[test]
fn malformed_profiles_and_narrowing_contracts_refuse_at_import() {
    for case in 0..15 {
        let mut doc = document();
        match case {
            0 => doc["operations"][0]["streaming"] = false.into(),
            1 => {
                doc["operations"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("control");
            }
            2 => doc["operations"][0]["source_profile"]["stop"] = "restart".into(),
            3 => doc["operations"][0]["source_profile"]["ready_wait_ms"] = 0.into(),
            4 => doc["operations"][0]["source_profile"]["tail_ms"] = 60001.into(),
            5 => doc["operations"][0]["source_profile"]["default"] = "true".into(),
            6 => {
                doc["operations"][0]["source_profile"]["narrowing"]["duration"]["maximum_from"] =
                    serde_json::json!(["unfixed"])
            }
            7 => {
                doc["operations"][0]["source_profile"]["narrowing"]["duration"]["maximum_from"] =
                    serde_json::json!(["config", "absent"])
            }
            8 => {
                doc["operations"][0]["source_profile"]["narrowing"]["filter"]["parameter"] =
                    "duration_ms".into()
            }
            9 => doc["operations"][0]["parameters"][1]["required"] = true.into(),
            10 => {
                doc["operations"][0]["source_profile"]["narrowing"]
                    .as_object_mut()
                    .unwrap()
                    .remove("duration");
            }
            11 => doc["operations"][0]["fixed_arguments"]["config"]["duration"] = (-1).into(),
            12 => {
                doc["operations"][0]["source_profile"]["narrowing"]["duration"]["maximum"] =
                    40000.into()
            }
            13 => {
                let mut other = doc["operations"][0].clone();
                other["path"] = serde_json::json!(["another"]);
                doc["operations"].as_array_mut().unwrap().push(other);
            }
            14 => {
                doc["operations"][0]["source_profile"]["narrowing"]["packets"]["parameter"] =
                    "duration_ms".into()
            }
            _ => unreachable!(),
        }
        assert!(
            super::super::schema::compile(&doc.to_string(), None).is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn direct_source_call_cannot_bypass_profile_ceilings_before_executable_entry() {
    let definition = super::super::schema::compile(&document().to_string(), None).unwrap();
    let product =
        super::super::session::unbound(definition, crate::process::ProcessConfig::default())
            .unwrap();
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
        .start(std::time::Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    for n in [0, -1, 30001] {
        let call = Call {
            run: run.clone(),
            authority: Default::default(),
            capability: product.description().capabilities().next().unwrap().clone(),
            arguments: [("duration_ms".into(), integer(n))].into(),
        };
        let result = streams::spawn(
            call,
            product.streams().unwrap().clone(),
            Default::default(),
            Default::default(),
            CancellationToken::new(),
        );
        assert!(matches!(result, Err(streams::StreamError::InvalidResource)));
    }
}

#[test]
fn captured_profile_reconstruction_never_reads_new_defaults_or_ceilings_from_disk() {
    let root = tempfile::tempdir().unwrap();
    let mut doc = document();
    doc["backend"]["bin"] = std::env::current_exe().unwrap().to_str().unwrap().into();
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let importer = super::super::ProviderImporter::new(root.path(), Default::default()).unwrap();
    let request = ImportRequest::new(
        "provider".into(),
        None,
        [(
            "file".into(),
            Value::new(
                wes_core::Shape::Unknown,
                Data::Text("provider.yaml".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into(),
    )
    .unwrap();
    let recipe = importer.capture(&request, 256 * 1024).unwrap();
    doc["operations"][0]["source_profile"]["name"] = "changed".into();
    doc["operations"][0]["source_profile"]["default"] = false.into();
    doc["operations"][0]["fixed_arguments"]["config"]["duration"] = 100000.into();
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let (old, _) =
        super::super::importer::reconstruct(recipe.source(), false, Some("renamed_alias")).unwrap();
    let original = old.operations[0].source_profile.as_ref().unwrap();
    assert_eq!(original.name(), "metrics-lab");
    assert!(original.is_default());
    assert!(matches!(
        original.options()["duration"].restriction,
        Restriction::Maximum(30000)
    ));
    let new_recipe = importer.capture(&request, 256 * 1024).unwrap();
    let (new, _) = super::super::importer::reconstruct(new_recipe.source(), false, None).unwrap();
    assert_eq!(
        new.operations[0].source_profile.as_ref().unwrap().name(),
        "changed"
    );
    assert!(matches!(
        new.operations[0].source_profile.as_ref().unwrap().options()["duration"].restriction,
        Restriction::Maximum(100000)
    ));
}
