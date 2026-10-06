//! Input retention uses synthetic files and bounded loopback fixtures, never live user sources.
#[path = "support/python.rs"]
mod python;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use wes::data_home::{DataHome, sources::Sources};
use wes_adapters::{
    api_library::{Library, PackageKey, digest},
    source_archive::{SourceArchive, SourceKind},
    type_sources::FileTypeSources,
};
use wes_engine::type_sources::{TypeSourceError, TypeSourceReader};

fn home() -> (tempfile::TempDir, PathBuf, Arc<Sources>) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap().join("data");
    drop(DataHome::open(&home).unwrap());
    let archive = Arc::new(Sources::new(home.clone()));
    (temp, home, archive)
}
fn records(home: &Path) -> Vec<Value> {
    fs::read_dir(home.join("imports/records"))
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect()
}

#[test]
fn yaml_inputs_deduplicate_bytes_not_names_and_preserve_changed_revisions() {
    let (temp, home, archive) = home();
    let reader = FileTypeSources::new(temp.path())
        .unwrap()
        .with_archive(archive);
    let a = "types: {First: {base: Text}}\n";
    let b = "types: {Second: {base: Int}}\n";
    fs::write(temp.path().join("same.yaml"), a).unwrap();
    assert_eq!(reader.read("same.yaml", 1024).unwrap(), a);
    let object = home.join(format!("imports/packages/{}.yaml", digest(a.as_bytes())));
    let before = fs::metadata(&object).unwrap().modified().unwrap();
    assert_eq!(reader.read("same.yaml", 1024).unwrap(), a);
    assert_eq!(records(&home).len(), 1);
    fs::write(temp.path().join("renamed.yaml"), a).unwrap();
    assert_eq!(reader.read("renamed.yaml", 1024).unwrap(), a);
    assert_eq!(records(&home).len(), 2);
    assert_eq!(fs::metadata(&object).unwrap().modified().unwrap(), before);
    fs::write(temp.path().join("same.yaml"), b).unwrap();
    assert_eq!(reader.read("same.yaml", 1024).unwrap(), b);
    assert_eq!(fs::read_to_string(&object).unwrap(), a);
    assert_eq!(
        fs::read_to_string(home.join(format!("imports/packages/{}.yaml", digest(b.as_bytes()))))
            .unwrap(),
        b
    );
    assert_eq!(records(&home).len(), 3);
    assert_eq!(
        fs::read_dir(home.join("imports/packages")).unwrap().count(),
        2
    );
    for record in records(&home) {
        assert!(!Path::new(record["object"].as_str().unwrap()).is_absolute());
        assert!(home.join(record["object"].as_str().unwrap()).exists());
    }
}

#[test]
fn spec_capture_reuses_catalog_object_without_changing_acceptance_or_origin() {
    let (_temp, home, archive) = home();
    let source = include_str!("../../../examples/api-library/repository/descriptor.json");
    let key = PackageKey {
        service: "fixture".into(),
        api_version: "v1".into(),
        scope: "default".into(),
    };
    let mut library = Library::open(&home.join("api-library")).unwrap();
    let package = library
        .save(
            key.clone(),
            source.as_bytes(),
            "fixture-repository".into(),
            None,
            true,
        )
        .unwrap();
    let object = library.descriptor_path(&package.revision).unwrap();
    let before = fs::metadata(&object).unwrap().modified().unwrap();
    drop(library);
    for origin in ["/first/spec.json", "/second/spec.json", "/first/spec.json"] {
        archive
            .retain(SourceKind::Spec, origin, "spec/json/v1", source)
            .unwrap();
    }
    assert_eq!(
        fs::read_dir(home.join("api-library/objects"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(fs::metadata(&object).unwrap().modified().unwrap(), before);
    assert_eq!(records(&home).len(), 2);
    let library = Library::open(&home.join("api-library")).unwrap();
    let current = library.find(&key, None).unwrap();
    assert!(current.accepted);
    assert_eq!(current.origin, "fixture-repository");
    assert_eq!(current.revision, package.revision);
}

#[test]
fn persistence_failure_corruption_and_symlinks_refuse_capture_without_overwrite() {
    let (temp, home, archive) = home();
    let reader = FileTypeSources::new(temp.path())
        .unwrap()
        .with_archive(archive.clone());
    let source = "types: {}\n";
    fs::write(temp.path().join("input.yaml"), source).unwrap();
    fs::write(home.join("imports"), "blocking file").unwrap();
    assert_eq!(
        reader.read("input.yaml", 1024),
        Err(TypeSourceError::Persistence)
    );
    fs::remove_file(home.join("imports")).unwrap();
    reader.read("input.yaml", 1024).unwrap();
    let object = home.join(format!(
        "imports/packages/{}.yaml",
        digest(source.as_bytes())
    ));
    fs::write(&object, "corrupt").unwrap();
    assert_eq!(
        reader.read("input.yaml", 1024),
        Err(TypeSourceError::Persistence)
    );
    assert_eq!(fs::read_to_string(&object).unwrap(), "corrupt");
    #[cfg(unix)]
    {
        fs::remove_file(&object).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, source).unwrap();
        std::os::unix::fs::symlink(&outside, &object).unwrap();
        assert_eq!(
            reader.read("input.yaml", 1024),
            Err(TypeSourceError::Persistence)
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), source);
    }
    assert_eq!(reader.read("input.yaml", 1), Err(TypeSourceError::TooLarge));
}

#[test]
fn archive_receives_the_same_once_read_bytes_that_the_engine_receives() {
    struct ChangeAfterCapture {
        archive: Arc<Sources>,
        original: PathBuf,
    }
    impl SourceArchive for ChangeAfterCapture {
        fn retain(
            &self,
            kind: SourceKind,
            origin: &str,
            format: &str,
            source: &str,
        ) -> std::io::Result<()> {
            self.archive.retain(kind, origin, format, source)?;
            fs::write(&self.original, "types: {Changed: {base: Int}}")?;
            Ok(())
        }
    }
    let (temp, home, archive) = home();
    let source = "types: {Original: {base: Text}}";
    let original = temp.path().join("input.yaml");
    fs::write(&original, source).unwrap();
    let reader = FileTypeSources::new(temp.path())
        .unwrap()
        .with_archive(Arc::new(ChangeAfterCapture { archive, original }));
    assert_eq!(reader.read("input.yaml", 1024).unwrap(), source);
    assert_eq!(
        fs::read_to_string(home.join(format!(
            "imports/packages/{}.yaml",
            digest(source.as_bytes())
        )))
        .unwrap(),
        source
    );
}

#[test]
fn actual_example_restores_yaml_views_and_specs_without_original_files() {
    let result = python::command()
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/import-retention/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn managed_environment_file_import_retains_original_yaml_and_relative_spec() {
    use wes_engine::environments::EnvironmentLoader;
    let (temp, home, archive) = home();
    let inputs = temp.path().join("inputs");
    fs::create_dir(&inputs).unwrap();
    let spec = include_str!("../../../examples/api-library/repository/descriptor.json");
    fs::write(inputs.join("spec.json"), spec).unwrap();
    fs::write(
        inputs.join("environment.yaml"),
        r#"
version: 1
targets:
  local: {kind: local}
environments:
  demo:
    imports:
      inventory:
        source: {kind: spec, file: spec.json}
        bind: {target: local, endpoint: 'http://127.0.0.1:1'}
"#,
    )
    .unwrap();
    let loader = wes_adapters::environments::LocalEnvironments::new(temp.path())
        .unwrap()
        .with_archive(archive);
    let captured = loader.capture("inputs/environment.yaml").unwrap();
    assert_eq!(captured.sources.iter().next().unwrap().1.bytes(), spec);
    let receipts = records(&home);
    assert_eq!(receipts.len(), 2);
    let environment = receipts
        .iter()
        .find(|r| r["kind"] == "environments")
        .unwrap();
    assert_eq!(environment["format"], "environments/yaml/v1");
    assert_eq!(
        fs::read(home.join(environment["object"].as_str().unwrap())).unwrap(),
        fs::read(inputs.join("environment.yaml")).unwrap()
    );
    let spec_receipt = receipts.iter().find(|r| r["kind"] == "spec").unwrap();
    // Compared as a path: its separators are the host's.
    assert!(Path::new(spec_receipt["origin"].as_str().unwrap()).ends_with("inputs/spec.json"));
    assert_eq!(
        fs::read_to_string(home.join(spec_receipt["object"].as_str().unwrap())).unwrap(),
        spec
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn url_capture_services_archive_exact_ready_descriptor_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wes_adapters::{
        credentials::MemoryCredentials,
        imports::{SpecDocuments, SpecImporter},
    };
    use wes_core::{Data, Shape};
    use wes_engine::imports::{ImportError, ImportRecipe, ImportRequest, Importer};
    let (temp, home, archive) = home();
    let source = include_str!("../../../examples/import-retention/service.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/spec.json", listener.local_addr().unwrap());
    let serve = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            while !buffer.windows(4).any(|p| p == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                buffer.extend_from_slice(&bytes[..n]);
                assert!(buffer.len() < 8192);
            }
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        source.len(),
                        source
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        })
        .await
        .unwrap();
    });
    let origin = url.clone();
    let base = temp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        fn request(url: &str) -> ImportRequest {
            ImportRequest::new(
                "spec".into(),
                None,
                [(
                    "url".into(),
                    wes_core::Value::new(
                        Shape::Unknown,
                        Data::Text(url.into()),
                        Default::default(),
                    )
                    .unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap()
        }
        let credentials = Arc::new(MemoryCredentials::with_environment(
            Default::default(),
            |_| Ok(None),
        ));
        let importer = SpecImporter::new(&base, credentials.clone(), Default::default())
            .unwrap()
            .with_archive(archive.clone());
        assert_eq!(
            importer.capture(&request(&url), 1024).unwrap().source(),
            source
        );
        struct Documents;
        impl SpecDocuments for Documents {
            fn capture(&self, _: &str, _: usize) -> Result<ImportRecipe, ImportError> {
                ImportRecipe::new(
                    "spec/json/v1".into(),
                    include_str!("../../../examples/api-library/repository/descriptor.json").into(),
                )
            }
        }
        let importer = SpecImporter::new(base, credentials, Default::default())
            .unwrap()
            .with_documents(Arc::new(Documents))
            .with_archive(archive);
        let recipe = importer
            .capture(
                &request("https://synthetic.invalid/ready.json"),
                1024 * 1024,
            )
            .unwrap();
        assert_eq!(recipe.format(), "spec/json/v1");
    })
    .await
    .unwrap();
    serve.await.unwrap();
    let receipts = records(&home);
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().any(|r| r["origin"] == origin));
    assert!(receipts.iter().any(|r| r["format"] == "spec/json/v1"));
    for receipt in receipts {
        let bytes = fs::read(home.join(receipt["object"].as_str().unwrap())).unwrap();
        assert_eq!(digest(&bytes), receipt["revision"].as_str().unwrap());
    }
}

#[tokio::test]
async fn storage_failure_refuses_definitions_and_keeps_original_command_in_history() {
    use wes::runtime::{RuntimeOptions, launch};
    use wes_engine::source::SourceInput;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("data");
    fs::write(
        temp.path().join("input.yaml"),
        "types: {NotRegistered: {base: Text}}",
    )
    .unwrap();
    fs::write(
        temp.path().join("spec.json"),
        include_str!("../../../examples/import-retention/service.json"),
    )
    .unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    fs::write(home.join("imports"), "storage unavailable").unwrap();
    let session = runtime.handle.current().unwrap().session;
    for (id, source) in [
        ("types", ":package load path:\"input.yaml\""),
        ("spec", ":import spec file:\"spec.json\""),
    ] {
        session
            .submit(SourceInput::new(id.into(), source.into()).unwrap())
            .await
            .unwrap();
    }
    let checkpoint = session.checkpoint().await.unwrap();
    let observation = session.observe().await.unwrap();
    assert_eq!(observation.cells.len(), 2);
    assert!(!observation.types.iter().any(|t| t == "NotRegistered"));
    for (cell, source) in observation.cells.iter().zip([
        ":package load path:\"input.yaml\"",
        ":import spec file:\"spec.json\"",
    ]) {
        assert_eq!(cell.input.text(), source);
        let reply = cell.reply.as_ref().unwrap().as_ref().unwrap();
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.message.contains("could not be saved")),
            "{:?}",
            reply.diagnostics
        );
    }
    checkpoint.resume().await;
    runtime.shutdown().await.unwrap();
}

#[test]
fn environment_archive_preserves_exact_bytes_and_versions_across_source_paths() {
    use wes_engine::environments::EnvironmentLoader;
    let (temp, home, archive) = home();
    let loader = wes_adapters::environments::LocalEnvironments::new(temp.path())
        .unwrap()
        .with_archive(archive);
    let source = "# original comment\nversion: 1\nenvironments: {demo: {}}\n";
    let object = home.join(format!(
        "imports/packages/{}.yaml",
        digest(source.as_bytes())
    ));
    fs::write(temp.path().join("environments.yaml"), source).unwrap();
    loader.capture("environments.yaml").unwrap();
    let before = fs::metadata(&object).unwrap().modified().unwrap();
    loader.capture("environments.yaml").unwrap();
    assert_eq!(records(&home).len(), 1);
    fs::write(temp.path().join("renamed.yaml"), source).unwrap();
    loader.capture("renamed.yaml").unwrap();
    assert_eq!(fs::metadata(&object).unwrap().modified().unwrap(), before);
    let changed = source.replace("original comment", "changed comment");
    fs::write(temp.path().join("environments.yaml"), &changed).unwrap();
    loader.capture("environments.yaml").unwrap();
    assert_eq!(fs::read_to_string(&object).unwrap(), source);
    assert_eq!(
        fs::read_to_string(home.join(format!(
            "imports/packages/{}.yaml",
            digest(changed.as_bytes())
        )))
        .unwrap(),
        changed
    );
    assert_eq!(
        fs::read_dir(home.join("imports/packages")).unwrap().count(),
        2
    );
    assert_eq!(records(&home).len(), 3);
    for receipt in records(&home) {
        assert_eq!(receipt["kind"], "environments");
        assert_eq!(receipt["format"], "environments/yaml/v1");
        assert!(!Path::new(receipt["object"].as_str().unwrap()).is_absolute());
        assert_eq!(
            Path::new(receipt["origin"].as_str().unwrap())
                .parent()
                .unwrap(),
            temp.path().canonicalize().unwrap()
        );
    }
}

#[tokio::test]
async fn environment_archive_failure_refuses_admission_without_overwriting() {
    use wes::runtime::{RuntimeOptions, launch};
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("data");
    let source = "version: 1\nenvironments: {NotRegistered: {}}\n";
    fs::write(temp.path().join("environments.yaml"), source).unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let before = session.environment_revisions().await.unwrap();
    fs::write(home.join("imports"), "storage unavailable").unwrap();
    assert!(
        session
            .plan_environment_file("environments.yaml".into(), false)
            .await
            .is_err()
    );
    assert_eq!(session.environment_revisions().await.unwrap(), before);
    fs::remove_file(home.join("imports")).unwrap();
    fs::create_dir_all(home.join("imports/packages")).unwrap();
    let object = home.join(format!(
        "imports/packages/{}.yaml",
        digest(source.as_bytes())
    ));
    fs::write(&object, "corrupt").unwrap();
    assert!(
        session
            .plan_environment_file("environments.yaml".into(), false)
            .await
            .is_err()
    );
    assert_eq!(session.environment_revisions().await.unwrap(), before);
    assert_eq!(fs::read_to_string(object).unwrap(), "corrupt");
    runtime.shutdown().await.unwrap();
}
