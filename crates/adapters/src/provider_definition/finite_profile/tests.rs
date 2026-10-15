use super::*;
use serde_json::{Value as Json, json};
use wes_core::{Data, Shape};
use wes_engine::imports::{ImportRequest, Importer};
fn document() -> Json {
    json!({"version":1,"provider":"processor","backend":{"kind":"process","bin":"/synthetic/not-invoked"},
        "types":{"Count":{"base":"Int","min":1,"max":3}},
        "operations":[{"path":["work"],"parameters":[{"name":"n","type":"Count"}],
        "argv":[{"parameter":"n"}],"result":{"format":"json","type":"Int"},
        "finite_profile":{"name":"bounded","kind":"sample.decode","arguments":{"items":"n"}}}]})
}
#[test]
fn typed_finite_metadata_is_available_without_running_its_process() {
    let definition = super::super::schema::compile(&document().to_string(), Some("alias")).unwrap();
    let profile = definition.operations[0].finite_profile.as_ref().unwrap();
    assert_eq!(profile.name(), "bounded");
    assert_eq!(profile.arguments()["items"].parameter, "n");
    assert!(
        !profile.arguments()["items"]
            .contract
            .issues(&Data::Int(0))
            .is_empty()
    );
    let (base, invoker) =
        crate::process::wrapping("alias", "/synthetic/not-invoked", Default::default()).unwrap();
    let product = super::super::invocation::wrap(
        definition,
        &base,
        Arc::new(invoker),
        Default::default(),
        vec![],
    )
    .unwrap();
    let capability = product.description().capabilities().next().unwrap();
    assert_eq!(
        product.invoker().finite_profile(capability).unwrap().kind(),
        "sample.decode"
    );
}
#[test]
fn malformed_incomplete_fixed_duplicate_and_streaming_profiles_refuse_at_import() {
    for case in 0..9 {
        let mut doc = document();
        match case {
            0 => doc["operations"][0]["finite_profile"]["name"] = "bad name".into(),
            1 => doc["operations"][0]["finite_profile"]["kind"] = "x".repeat(65).into(),
            2 => doc["operations"][0]["finite_profile"]["arguments"] = json!({}),
            3 => doc["operations"][0]["finite_profile"]["arguments"] = json!({"items":"missing"}),
            4 => doc["operations"][0]["finite_profile"]["arguments"] = json!({"a":"n","b":"n"}),
            5 => doc["operations"][0]["fixed_arguments"] = json!({"n":2}),
            6 => doc["operations"][0]["finite_profile"]["default"] = true.into(),
            7 => {
                let mut op = doc["operations"][0].clone();
                op["path"] = json!(["other"]);
                doc["operations"].as_array_mut().unwrap().push(op);
            }
            _ => {
                doc["backend"]["kind"] = "session".into();
                doc["operations"][0]["streaming"] = true.into();
            }
        }
        assert!(
            super::super::schema::compile(&doc.to_string(), None).is_err(),
            "case {case}"
        );
    }
}
#[test]
fn finite_profile_reconstruction_keeps_the_captured_mapping_and_contracts() {
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
                Shape::Unknown,
                Data::Text("provider.yaml".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into(),
    )
    .unwrap();
    let captured = importer.capture(&request, 256 * 1024).unwrap();
    doc["operations"][0]["finite_profile"]["name"] = "changed".into();
    doc["operations"][0]["finite_profile"]["arguments"] = json!({"amount":"n"});
    doc["types"]["Count"]["max"] = 30.into();
    std::fs::write(root.path().join("provider.yaml"), doc.to_string()).unwrap();
    let (old, _) =
        super::super::importer::reconstruct(captured.source(), false, Some("renamed")).unwrap();
    let profile = old.operations[0].finite_profile.as_ref().unwrap();
    assert_eq!(profile.name(), "bounded");
    assert!(profile.arguments().contains_key("items"));
    assert!(
        !profile.arguments()["items"]
            .contract
            .issues(&Data::Int(4))
            .is_empty()
    );
    let current = importer.capture(&request, 256 * 1024).unwrap();
    let (new, _) = super::super::importer::reconstruct(current.source(), false, None).unwrap();
    assert_eq!(
        new.operations[0].finite_profile.as_ref().unwrap().name(),
        "changed"
    );
}
