use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wes_adapters::{
    http::HttpConfig,
    imports::{OpenApiCompiler, OpenApiImporter, SpecImporter},
};
use wes_core::{Data, Shape, Value};
use wes_engine::{
    credentials::{CredentialError, Credentials, Secret},
    imports::{ImportError, ImportMode, ImportRecipe, ImportRequest, ImportSnapshot, Importer},
};

#[derive(Default)]
struct Secrets(AtomicUsize);
impl Credentials for Secrets {
    fn lookup(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
struct Compiler(AtomicUsize);
impl OpenApiCompiler for Compiler {
    fn compile(&self, source: &[u8]) -> Result<Vec<u8>, ImportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(source, b"synthetic captured OpenAPI source");
        Ok(serde_json::to_vec(&serde_json::json!({"version":1,"provider":"api","types":{},"servers":[{"url":"https://never-call.invalid"}],"source":{"provenance":{"entries":[{"target":"#/operations/0/auth","basis":"unknown"}]}},"diagnostics":["PRIVATE_SOURCE_ADVISORY"],"operations":[{"path":["health"],"method":"GET","route":"/health","auth":[{"query":"PRIVATE_QUERY","secret":"PRIVATE_SECRET"}],"parameters":[],"responses":{"200":"Text"}}]})).unwrap())
    }
}
fn request(arguments: &[(&str, &str)]) -> ImportRequest {
    ImportRequest::new(
        "openapi".into(),
        Some("demo".into()),
        arguments
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    Value::new(
                        Shape::Unknown,
                        Data::Text((*value).into()),
                        Default::default(),
                    )
                    .unwrap(),
                )
            })
            .collect(),
    )
    .unwrap()
}
#[test]
fn openapi_capture_checks_inputs_before_io_and_replay_never_reconverts_or_reads_credentials() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(Secrets::default());
    let compiler = Arc::new(Compiler(AtomicUsize::new(0)));
    let spec =
        Arc::new(SpecImporter::new(root.path(), secrets.clone(), HttpConfig::default()).unwrap());
    let importer = OpenApiImporter::new(spec, compiler.clone());
    assert!(
        matches!(importer.capture(&request(&[("file","absent.yaml")]), 1024*1024), Err(ImportError::MissingArgument(key)) if key == "endpoint")
    );
    assert!(matches!(
        importer.capture(
            &request(&[
                ("file", "absent.yaml"),
                ("url", "http://127.0.0.1:1"),
                ("endpoint", "http://127.0.0.1:1")
            ]),
            1024 * 1024
        ),
        Err(ImportError::ArgumentChoice(_))
    ));
    assert_eq!(compiler.0.load(Ordering::SeqCst), 0);
    std::fs::write(
        root.path().join("api.yaml"),
        b"synthetic captured OpenAPI source",
    )
    .unwrap();
    let request = request(&[("file", "api.yaml"), ("endpoint", "http://127.0.0.1:1")]);
    let recipe = importer.capture(&request, 1024 * 1024).unwrap();
    std::fs::remove_file(root.path().join("api.yaml")).unwrap();
    let snapshot = ImportSnapshot::new(request.clone(), recipe.clone());
    let live = importer.build(&snapshot, ImportMode::Live).unwrap();
    assert_eq!(live.description().name(), "demo");
    assert!(
        live.warnings()
            .iter()
            .any(|w| w.diagnostic(wes_language::Span::at(0)).code == "IMP008")
    );
    let lookups = secrets.0.load(Ordering::SeqCst);
    let restored = importer.build(&snapshot, ImportMode::Replay).unwrap();
    assert_eq!(restored.description().name(), "demo");
    assert_eq!(compiler.0.load(Ordering::SeqCst), 1);
    assert_eq!(secrets.0.load(Ordering::SeqCst), lookups);
    for warning in restored.warnings() {
        assert!(
            !warning
                .diagnostic(wes_language::Span::at(0))
                .public_summary()
                .contains("PRIVATE")
        );
    }
    let mut envelope: serde_json::Value = serde_json::from_str(recipe.source()).unwrap();
    envelope["descriptor"] = serde_json::json!("{}");
    let corrupted = ImportSnapshot::new(
        request,
        ImportRecipe::new(recipe.format().into(), envelope.to_string()).unwrap(),
    );
    assert!(matches!(
        importer.build(&corrupted, ImportMode::Replay),
        Err(ImportError::InvalidRecipe)
    ));
    assert_eq!(compiler.0.load(Ordering::SeqCst), 1);
}

#[test]
fn undocumented_authentication_is_an_advisory_not_a_credential_or_access_grant() {
    let secrets = Arc::new(Secrets::default());
    let descriptor = serde_json::json!({"version":1,"provider":"fixture","types":{},"source":{"provenance":{"entries":[{"target":"#/operations/0/auth","basis":"unknown"}]}},"operations":[{"path":["health"],"method":"GET","route":"/health","auth":[],"parameters":[],"responses":{"200":"Text"}}]});
    let reading = wes_adapters::descriptor::read(
        &serde_json::to_vec(&descriptor).unwrap(),
        None,
        secrets.clone(),
        HttpConfig::default(),
        "http://127.0.0.1:1",
    )
    .unwrap();
    assert!(
        reading
            .warnings
            .iter()
            .any(|warning| warning.diagnostic(wes_language::Span::at(0)).code == "IMP010")
    );
    assert!(reading.description.secrets().is_empty());
    assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
    let Some(Data::Record(info)) = reading.description.information() else {
        panic!("information")
    };
    let Data::List(auth) = &info["authentication"] else {
        panic!("authentication")
    };
    let Data::Record(auth) = &auth[0] else {
        panic!("operation")
    };
    assert_eq!(auth["state"], Data::Text("fixed".into()));
}
