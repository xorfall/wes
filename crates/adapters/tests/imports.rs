use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use wes_adapters::{
    http::HttpConfig,
    imports::{ProcessImporter, SpecImporter},
    process::ProcessConfig,
};
use wes_core::{Data, Shape, Value, capability::Safety};
use wes_engine::{
    credentials::{CredentialError, Credentials, Secret},
    driver::CancellationToken,
    imports::{
        ImportCapture, ImportError, ImportMode, ImportRecipe, ImportRequest, ImportSnapshot,
        Importer, Importers,
    },
};

#[derive(Default)]
struct CredentialsSpy {
    calls: AtomicUsize,
}
impl Credentials for CredentialsSpy {
    fn lookup(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
fn source(name: &str) -> String {
    serde_json::json!({"version":1,"provider":name,"types":{"Item":{"base":"Record","fields":{"id":"Int"}}},
        "operations":[{"path":["items"],"method":"GET","route":"/items","auth":[{"secret":"token"}],"parameters":[],"responses":{"200":"List<Item>"}}]}).to_string()
}
fn request(kind: &str, key: &str, path: &str) -> ImportRequest {
    let request = ImportRequest::new(
        kind.into(),
        None,
        [(
            key.into(),
            Value::new(Shape::Unknown, Data::Text(path.into()), Default::default()).unwrap(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    if kind == "spec" {
        let mut args = request.arguments().clone();
        args.insert(
            "endpoint".into(),
            Value::new(
                Shape::Unknown,
                Data::Text("https://example.invalid/api".into()),
                Default::default(),
            )
            .unwrap(),
        );
        ImportRequest::new(kind.into(), None, args).unwrap()
    } else {
        request
    }
}
fn registry(kind: &str, importer: impl Importer) -> Importers {
    let mut registry = Importers::default();
    registry.register(kind.into(), Arc::new(importer)).unwrap();
    registry
}

#[tokio::test]
async fn source_import_round_trips_through_history_and_replays_after_descriptor_removal() {
    history_round_trip(false).await;
}

#[tokio::test]
async fn url_import_round_trips_through_history_after_source_server_stops() {
    history_round_trip(true).await;
}

async fn history_round_trip(remote: bool) {
    use wes_adapters::codec::{
        Limits,
        history::{decode_journal, encode_journal},
    };
    use wes_engine::{
        history::JournalEntry,
        source::{SourceInput, SourcePreparation, prepare_declarations},
        type_sources::TypeSourceCapture,
        workspace::{ReplayWorkspace, Workspace},
    };
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let original = format!("\u{feff}{}\r\n", source("historic"));
    let descriptor = directory.path().join("description.json");
    fs::write(&descriptor, &original).unwrap();
    let (location, serving) = if remote {
        let (url, task) = serve_spec(original.as_bytes().to_vec(), "200 OK", false).await;
        (format!("url:{url:?}"), Some(task))
    } else {
        ("file:description.json".into(), None)
    };
    let secrets = Arc::new(CredentialsSpy::default());
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_importer(
            "spec".into(),
            Arc::new(
                SpecImporter::new(directory.path(), secrets.clone(), HttpConfig::default())
                    .unwrap(),
            ),
        )
        .unwrap();
    let prepared = prepare_declarations(
        SourceInput::new(
            "import".into(),
            format!(":import spec {location} endpoint:https://example.invalid/api as:library\nlibrary items > output"),
        )
        .unwrap(),
        workspace.draft().unwrap(),
        TypeSourceCapture::replay(Default::default()).unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let SourcePreparation::Declarations(prepared) = prepared else {
        panic!("declarations")
    };
    assert_eq!(prepared.accepted().len(), 2);
    assert!(workspace.catalogue().provider("library").is_none());
    let entry = JournalEntry::Command(prepared.record().unwrap().clone());
    let wire = encode_journal(&entry, Limits::default()).unwrap();
    let JournalEntry::Command(recorded) = decode_journal(&wire, Limits::default()).unwrap().entry
    else {
        panic!("command")
    };
    assert_eq!(recorded.imports[0].recipe().source(), original);
    if let Some(serving) = serving {
        serving.await.unwrap(); // Listener is gone before replay.
    }
    fs::remove_file(&descriptor).unwrap();
    let mut base = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    base.register_importer(
        "spec".into(),
        Arc::new(
            SpecImporter::new(elsewhere.path(), secrets.clone(), HttpConfig::default()).unwrap(),
        ),
    )
    .unwrap();
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let rebuilt = replay
        .prepare(&recorded, CancellationToken::new())
        .await
        .unwrap();
    assert!(replay.apply(rebuilt).unwrap().effects.is_empty());
    let mut restored = replay.finish();
    assert!(restored.catalogue().provider("library").is_some());
    assert_eq!(restored.resolve("output").unwrap().node, recorded.nodes[0]);
    assert!(restored.start(std::time::Duration::ZERO).is_empty());
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 1); // Only live import checked availability.
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

async fn serve_spec(
    body: Vec<u8>,
    status: &str,
    chunked: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/descriptor.json?version=1",
        listener.local_addr().unwrap()
    );
    let status = status.to_owned();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"GET /descriptor.json?version=1 HTTP/1.1\r\n"));
        let request = String::from_utf8(request).unwrap().to_lowercase();
        assert!(!request.contains("authorization:"));
        let mut response = if chunked {
            format!("HTTP/1.1 {status}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", body.len()).into_bytes()
        } else {
            format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: /redirected\r\nConnection: close\r\n\r\n", body.len()).into_bytes()
        };
        response.extend(body);
        if chunked {
            response.extend_from_slice(b"\r\n0\r\n\r\n");
        }
        let _ = socket.write_all(&response).await;
    });
    (url, task)
}

#[tokio::test]
async fn url_capture_rejects_http_errors_redirects_invalid_text_and_both_size_encodings() {
    for (body, status, chunked, limit, expected) in [
        (
            b"{}".to_vec(),
            "404 Not Found",
            false,
            100,
            ImportError::Unavailable,
        ),
        (
            b"{}".to_vec(),
            "302 Found",
            false,
            100,
            ImportError::Unavailable,
        ),
        (vec![0xff], "200 OK", false, 100, ImportError::InvalidRecipe),
        (vec![b' '; 101], "200 OK", false, 100, ImportError::Capacity),
        (vec![b' '; 101], "200 OK", true, 100, ImportError::Capacity),
    ] {
        let (url, task) = serve_spec(body, status, chunked).await;
        let result = tokio::task::spawn_blocking(move || {
            let root = tempfile::tempdir().unwrap();
            let importer = SpecImporter::new(
                root.path(),
                Arc::new(CredentialsSpy::default()),
                HttpConfig::default(),
            )
            .unwrap();
            importer.capture(&request("spec", "url", &url), limit)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), expected);
        task.await.unwrap();
    }
    let (url, task) = serve_spec(b"not JSON".to_vec(), "200 OK", false).await;
    let root = tempfile::tempdir().unwrap();
    let importer = SpecImporter::new(
        root.path(),
        Arc::new(CredentialsSpy::default()),
        HttpConfig::default(),
    )
    .unwrap();
    let mut capture = ImportCapture::live(registry("spec", importer));
    assert_eq!(
        capture
            .read(&request("spec", "url", &url), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Input("invalid or excessive JSON".into())
    );
    assert!(capture.finish().is_empty());
    task.await.unwrap();
}

#[test]
fn url_import_rejects_invalid_locations_and_conflicting_arguments_without_io() {
    let root = tempfile::tempdir().unwrap();
    let importer = SpecImporter::new(
        root.path(),
        Arc::new(CredentialsSpy::default()),
        HttpConfig::default(),
    )
    .unwrap();
    for location in [
        "file:///tmp/spec",
        "ftp://example.invalid/spec",
        "relative.json",
        "http://user:secret@example.invalid/spec",
        "https://example.invalid/spec#fragment",
        "https://example.invalid/\n",
    ] {
        assert_eq!(
            importer
                .capture(&request("spec", "url", location), 1024)
                .unwrap_err(),
            ImportError::InvalidRecipe
        );
    }
    let mut arguments = request("spec", "url", "http://127.0.0.1:1/spec")
        .arguments()
        .clone();
    arguments.extend(request("spec", "file", "absent.json").arguments().clone());
    let request = ImportRequest::new("spec".into(), None, arguments).unwrap();
    assert_eq!(
        importer.capture(&request, 1024).unwrap_err(),
        ImportError::InvalidRecipe
    );
}

#[tokio::test]
async fn spec_captures_exact_bytes_and_restores_after_removal_without_credentials_or_file_reads() {
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let original = format!("\u{feff}{}\r\n", source("historic"));
    fs::write(directory.path().join("description.json"), &original).unwrap();
    let secrets = Arc::new(CredentialsSpy::default());
    let importer =
        SpecImporter::new(directory.path(), secrets.clone(), HttpConfig::default()).unwrap();
    let input = request("spec", "file", "description.json");
    let mut capture = ImportCapture::live(registry("spec", importer));
    let captured = capture
        .read(&input, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(captured.product().description().name(), "historic");
    assert!(captured.product().streams().is_some());
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 1);
    assert!(
        captured
            .product()
            .warnings()
            .iter()
            .any(|s| s.contains("not available"))
    );
    fs::write(directory.path().join("description.json"), source("changed")).unwrap();
    assert!(Arc::ptr_eq(
        captured.product(),
        capture
            .read(&input, CancellationToken::new())
            .await
            .unwrap()
            .product()
    ));
    capture.accept(&captured).unwrap();
    let snapshots = capture.finish();
    assert_eq!(snapshots[0].recipe().source(), original);
    fs::remove_file(directory.path().join("description.json")).unwrap();
    let importer =
        SpecImporter::new(elsewhere.path(), secrets.clone(), HttpConfig::default()).unwrap();
    let mut replay = ImportCapture::replay(registry("spec", importer), snapshots).unwrap();
    let restored = replay.read(&input, CancellationToken::new()).await.unwrap();
    assert_eq!(restored.product().description().name(), "historic");
    assert!(restored.product().streams().is_some());
    assert_eq!(restored.product().description().secrets(), ["token"]);
    assert!(restored.product().warnings().is_empty());
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn malformed_spec_is_not_replaced_by_a_later_file_version_inside_one_capture() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("spec"), "not JSON").unwrap();
    let secrets = Arc::new(CredentialsSpy::default());
    let importer =
        SpecImporter::new(directory.path(), secrets.clone(), HttpConfig::default()).unwrap();
    let input = request("spec", "file", "spec");
    let mut capture = ImportCapture::live(registry("spec", importer));
    assert_eq!(
        capture
            .read(&input, CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Input("invalid or excessive JSON".into())
    );
    fs::write(directory.path().join("spec"), source("replacement")).unwrap();
    assert_eq!(
        capture
            .read(&input, CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Input("invalid or excessive JSON".into())
    );
    assert!(capture.finish().is_empty());
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn spec_input_limits_wrong_requests_and_formats_fail_without_credential_lookup() {
    let directory = tempfile::tempdir().unwrap();
    let secrets = Arc::new(CredentialsSpy::default());
    let importer =
        SpecImporter::new(directory.path(), secrets.clone(), HttpConfig::default()).unwrap();
    fs::write(directory.path().join("spec"), source("provider")).unwrap();
    fs::write(directory.path().join("binary"), [0xff]).unwrap();
    fs::write(directory.path().join("large"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    for (input, max, expected) in [
        (request("spec", "file", "spec"), 1, ImportError::Capacity),
        (
            request("spec", "file", "binary"),
            100,
            ImportError::InvalidRecipe,
        ),
        (
            request("spec", "file", "large"),
            usize::MAX,
            ImportError::Capacity,
        ),
        (request("spec", "file", "."), 100, ImportError::Unavailable),
        (
            request("spec", "file", "missing"),
            100,
            ImportError::Unavailable,
        ),
        (
            request("spec", "wrong", "spec"),
            100,
            ImportError::InvalidRecipe,
        ),
        (
            request("process", "file", "spec"),
            100,
            ImportError::InvalidRecipe,
        ),
        (
            request("spec", "file", "bad\npath"),
            100,
            ImportError::InvalidRecipe,
        ),
    ] {
        assert_eq!(importer.capture(&input, max).unwrap_err(), expected);
    }
    let wrong = ImportSnapshot::new(
        request("spec", "file", "missing"),
        ImportRecipe::new("spec/json/v2".into(), source("p")).unwrap(),
    );
    assert!(matches!(
        importer.build(&wrong, ImportMode::Replay),
        Err(ImportError::InvalidRecipe)
    ));
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn process_import_records_a_location_without_launch_and_replay_does_not_stat_it() {
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.exe");
    fs::write(
        &file,
        "Synthetic fixture, deliberately not an executable image",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let importer = ProcessImporter::new(directory.path(), ProcessConfig::default()).unwrap();
    let input = request("process", "bin", "fixture.exe");
    let mut capture = ImportCapture::live(registry("process", importer));
    let captured = capture
        .read(&input, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(captured.product().description().name(), "process");
    assert!(captured.product().conversations().is_some());
    assert!(captured.product().streams().is_none());
    assert_eq!(
        captured
            .product()
            .description()
            .capabilities()
            .next()
            .unwrap()
            .safety,
        Safety::Unsafe
    );
    capture.accept(&captured).unwrap();
    let snapshots = capture.finish();
    assert_eq!(
        std::path::Path::new(snapshots[0].recipe().source()),
        file.canonicalize().unwrap()
    );
    fs::remove_file(&file).unwrap();
    let importer = ProcessImporter::new(elsewhere.path(), ProcessConfig::default()).unwrap();
    let mut replay = ImportCapture::replay(registry("process", importer), snapshots).unwrap();
    let restored = replay.read(&input, CancellationToken::new()).await.unwrap();
    assert!(restored.product().conversations().is_some());
    let capability = restored
        .product()
        .description()
        .capabilities()
        .next()
        .unwrap();
    assert_eq!(capability.path, ["run"]);
    assert_eq!(capability.result, wes_adapters::process::output_shape());
    assert!(!file.exists());
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn process_rejects_missing_nonregular_nonexecutable_and_malformed_recipe_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let importer = ProcessImporter::new(directory.path(), ProcessConfig::default()).unwrap();
    for path in ["missing", "."] {
        assert_eq!(
            importer
                .capture(&request("process", "bin", path), 100)
                .unwrap_err(),
            ImportError::Unavailable
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = directory.path().join("nonexecutable");
        fs::write(&file, "fixture").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            importer
                .capture(&request("process", "bin", "nonexecutable"), 4096)
                .unwrap_err(),
            ImportError::Unavailable
        );
    }
    for (format, path) in [
        ("process/path/v1", "relative"),
        ("process/path/v2", "/absolute"),
        ("process/path/v1", "/bad\npath"),
    ] {
        let snapshot = ImportSnapshot::new(
            request("process", "bin", "file"),
            ImportRecipe::new(format.into(), path.into()).unwrap(),
        );
        assert!(matches!(
            importer.build(&snapshot, ImportMode::Replay),
            Err(ImportError::InvalidRecipe)
        ));
    }
}

#[cfg(unix)]
#[test]
fn ordinary_input_symlinks_keep_local_file_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("spec"), source("outside")).unwrap();
    std::os::unix::fs::symlink(external.path().join("spec"), directory.path().join("link"))
        .unwrap();
    let importer = SpecImporter::new(
        directory.path(),
        Arc::new(CredentialsSpy::default()),
        HttpConfig::default(),
    )
    .unwrap();
    let recipe = importer
        .capture(&request("spec", "file", "link"), 1024 * 1024)
        .unwrap();
    assert_eq!(recipe.source(), source("outside"));
}

#[test]
fn ready_descriptors_require_a_bound_endpoint_and_never_restore_retired_formats() {
    let directory = tempfile::tempdir().unwrap();
    let secrets = Arc::new(CredentialsSpy::default());
    let importer =
        SpecImporter::new(directory.path(), secrets.clone(), HttpConfig::default()).unwrap();
    let bound = request("spec", "file", "absent.json");
    let mut args = bound.arguments().clone();
    args.shift_remove("endpoint");
    let unbound = ImportRequest::new("spec".into(), None, args).unwrap();
    let mut current: serde_json::Value = serde_json::from_str(&source("current")).unwrap();
    current["servers"] =
        serde_json::json!([{"declaration":{"url":"https://documentation.invalid"}}]);
    let ready = ImportRecipe::new("spec/json/v1".into(), current.to_string()).unwrap();
    for mode in [ImportMode::Live, ImportMode::Replay] {
        assert!(matches!(
            importer.build(&ImportSnapshot::new(unbound.clone(), ready.clone()), mode),
            Err(ImportError::Input(message)) if message == "descriptor requires an explicit endpoint or environment bind.endpoint; documented servers are not execution destinations"
        ));
        let retired = ImportRecipe::new("spec/json/v1".into(), r#"{"provider":"old","base":"https://old.invalid","auth":{"secret":"token"},"capabilities":[{"path":["get"],"http":{"method":"GET","path":"/"}}]}"#.into()).unwrap();
        assert!(matches!(
            importer.build(&ImportSnapshot::new(bound.clone(), retired), mode),
            Err(ImportError::Input(message)) if message == "expected current descriptor fields (version, provider, types, operations)"
        ));
    }
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 0);
    assert!(
        importer
            .build(&ImportSnapshot::new(bound, ready), ImportMode::Replay)
            .is_ok()
    );
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn generated_receipts_are_rejected_even_with_explicit_destination() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(CredentialsSpy::default());
    let importer = SpecImporter::new(root.path(), secrets.clone(), HttpConfig::default()).unwrap();
    let input = request("spec", "file", "absent.json");
    for mode in [ImportMode::Live, ImportMode::Replay] {
        let recipe = ImportRecipe::new("spec/generated/v1".into(), source("fixture")).unwrap();
        assert!(matches!(
            importer.build(&ImportSnapshot::new(input.clone(), recipe), mode),
            Err(ImportError::InvalidRecipe)
        ));
    }
    assert_eq!(secrets.calls.load(Ordering::SeqCst), 0);
}
