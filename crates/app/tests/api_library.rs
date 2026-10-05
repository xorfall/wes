use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use wes::api_library::{ApiLibrary, parse_request};
use wes_adapters::api_library::{Library, PackageKey, digest};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    library: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        Self {
            home: root.join("home"),
            library: root.join("home/api-library"),
            root,
            _temporary: temporary,
        }
    }
    async fn action(&self, value: Value) -> std::io::Result<Value> {
        let service = ApiLibrary::new(self.home.clone());
        let bytes = serde_json::to_vec(&value).unwrap();
        tokio::task::spawn_blocking(move || service.perform(parse_request(&bytes)?))
            .await
            .unwrap()
    }
    fn draft_answer(&self, name: &str) -> PathBuf {
        let bytes = include_bytes!("../../../examples/api-import/inventory.json");
        let (text, source) = wes_adapters::api_library::draft::from_descriptor(bytes).unwrap();
        let path = self.root.join(name);
        std::fs::write(
            &path,
            serde_json::to_vec(
                &json!({"draft":serde_json::from_str::<Value>(&text).unwrap(),"source":source}),
            )
            .unwrap(),
        )
        .unwrap();
        path
    }
    async fn configure(&self, repo: Value) -> Value {
        self.action(json!({"action":"configure","expectedRevision":null,"settings":{"localDirectory":self.library,"repository":repo,"extractor":null}})).await.unwrap()
    }
    fn descriptor(&self, name: &str, change: bool) -> PathBuf {
        let mut value: Value = serde_json::from_slice(include_bytes!(
            "../../../examples/api-import/inventory.json"
        ))
        .unwrap();
        if change {
            value["operations"][0]["summary"] = json!("A revised public summary");
        }
        let path = self.root.join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        path
    }
}
fn key() -> Value {
    json!({"service":"inventory","apiVersion":"v1","scope":"default"})
}
fn native_key() -> PackageKey {
    serde_json::from_value(key()).unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn rust_descriptor_rejection_also_retains_its_specific_validation_reason() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let executable = f.root.join("invalid-output");
    std::fs::write(&executable, "#!/bin/sh\n/bin/cat >/dev/null\nprintf '{}'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    std::fs::write(f.root.join("openapi.json"), br#"{"openapi":"3.0.3"}"#).unwrap();
    let error = describe(
        &ApiLibrary::new(f.home.clone()),
        &f.root,
        "openapi.json".into(),
        false,
        None,
    )
    .await
    .unwrap_err();
    let wes_engine::providers::InvocationError::Failed(error) = error else {
        panic!("expected failure")
    };
    assert_eq!(error.code(), "DSC005");
    let report = f
        .action(json!({"action":"describeFailure","id":error.issues()[0].message}))
        .await
        .unwrap();
    assert!(!report["report"]["message"].as_str().unwrap().is_empty());
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn describe_failure_details_survive_service_recreation_without_public_disclosure() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let report = json!({"version":1,"message":"Synthetic private root cause","issues":[{"kind":"missing","operation":"GET /items","message":"Authentication is not documented.","lines":[{"start":2,"end":4}]}],"omitted":0});
    std::fs::write(
        f.root.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let executable = f.root.join("reject");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\necho raw-secret-stderr >&2\n/bin/cat '{}'\nexit 21\n",
            f.root.join("report.json").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    std::fs::write(f.root.join("openapi.json"), br#"{"openapi":"3.0.3"}"#).unwrap();
    let error = describe(
        &ApiLibrary::new(f.home.clone()),
        &f.root,
        "openapi.json".into(),
        false,
        None,
    )
    .await
    .unwrap_err();
    let wes_engine::providers::InvocationError::Failed(error) = error else {
        panic!("expected failure")
    };
    assert_eq!(error.code(), "DSC002");
    assert!(!error.message().contains("private root cause"));
    let reference = &error.issues()[0];
    assert_eq!(reference.code, "DSC_REPORT");
    assert!(
        error.issues().iter().any(|issue| issue.code == "DSC_REASON"
            && issue.message.contains("Required API definitions"))
    );
    assert!(!format!("{:?}", error.issues()).contains("Authentication is not documented."));
    let details = f
        .action(json!({"action":"describeFailure","id":reference.message}))
        .await
        .unwrap();
    assert_eq!(details["report"], report);
    assert!(!details.to_string().contains("raw-secret-stderr"));
    let file = f
        .home
        .join("diagnostics/describe")
        .join(format!("{}.json", reference.message));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
    assert!(
        f.action(json!({"action":"describeFailure","id":"../../settings"}))
            .await
            .is_err()
    );
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(f.root.join("report.json"), &file).unwrap();
    assert!(
        f.action(json!({"action":"describeFailure","id":reference.message}))
            .await
            .is_err()
    );

    // A report-write failure must not replace the original extraction category.
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_dir(f.home.join("diagnostics/describe")).unwrap();
    std::fs::write(f.home.join("diagnostics/describe"), "blocked directory").unwrap();
    let error = describe(
        &ApiLibrary::new(f.home.clone()),
        &f.root,
        "openapi.json".into(),
        false,
        None,
    )
    .await
    .unwrap_err();
    let wes_engine::providers::InvocationError::Failed(error) = error else {
        panic!("expected failure")
    };
    assert_eq!(error.code(), "DSC002");
    assert!(error.message().contains("could not be saved"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn failed_or_invalid_extraction_publishes_no_revision_or_source() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let executable = f.root.join("synthetic-extractor");
    std::fs::write(&executable, "#!/bin/sh\n/bin/cat >/dev/null\nprintf '{}'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    let source = f.root.join("source.md");
    std::fs::write(
        &source,
        "Entire synthetic README\nLimit: at most 100 results.\n",
    )
    .unwrap();
    let request = json!({"action":"ingest","key":key(),"source":{"location":source}});
    assert!(f.action(request.clone()).await.is_err());
    std::fs::write(
        &executable,
        "#!/bin/sh\n/bin/cat >/dev/null\nprintf 'synthetic failure' >&2\nexit 1\n",
    )
    .unwrap();
    assert!(f.action(request).await.is_err());
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
    for folder in ["objects", "sources"] {
        assert_eq!(
            std::fs::read_dir(f.library.join(folder)).unwrap().count(),
            0
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_is_required_shared_and_cas_checked_without_repo_dependency() {
    let f = Fixture::new();
    assert_eq!(
        f.action(json!({"action":"status"})).await.unwrap()["settings"],
        Value::Null
    );
    assert!(f.action(json!({"action":"list"})).await.is_err());
    for path in [
        json!("relative"),
        json!(f.home),
        json!(f.root.join("outside")),
    ] {
        assert!(
            f.action(json!({"action":"configure","settings":{"localDirectory":path}}))
                .await
                .is_err()
        );
    }
    let state = f.configure(Value::Null).await;
    assert_eq!(f.action(json!({"action":"status"})).await.unwrap(), state);
    assert_eq!(state["managed"], true);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(f.home.join("api-library-settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["settings"]["localDirectory"], "api-library");
    assert!(
        f.action(json!({"action":"configure","settings":{"localDirectory":f.library}}))
            .await
            .is_err()
    );
    let changed=f.action(json!({"action":"configure","expectedRevision":state["revision"],"settings":{"localDirectory":f.library,"repository":null,"extractor":null}})).await.unwrap();
    assert_eq!(state, changed);
    assert!(f.action(json!({"action":"configure","expectedRevision":state["revision"],"settings":{"localDirectory":f.library,"repository":{"kind":"local","directory":f.root}}})).await.is_err());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn direct_library_rejects_corrupt_or_linked_settings_without_changing_them() {
    use std::os::unix::fs::symlink;
    for bytes in [
        br#"{"version":1,"version":1,"settings":{"localDirectory":"api-library"}}"#.to_vec(),
        br#"{"version":2,"settings":{"localDirectory":"api-library"}}"#.to_vec(),
        br#"{"version":1,"settings":{"localDirectory":"api-library","repository":{"kind":"private-setting-marker"}}}"#.to_vec(),
        vec![b' '; 16 * 1024 + 1],
    ] {
        let f = Fixture::new();
        std::fs::create_dir(&f.home).unwrap();
        let file = f.home.join("api-library-settings.json");
        std::fs::write(&file, &bytes).unwrap();
        let error = f.action(json!({"action":"status"})).await.unwrap_err();
        assert!(!error.to_string().contains("private-setting-marker"));
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert!(!f.library.exists());
    }
    for link_settings in [true, false] {
        let f = Fixture::new();
        std::fs::create_dir(&f.home).unwrap();
        let file = f.home.join("api-library-settings.json");
        let bytes = br#"{"version":1,"settings":{"localDirectory":"api-library"}}"#;
        let outside = f.root.join("outside");
        if link_settings {
            std::fs::write(&outside, bytes).unwrap();
            symlink(&outside, &file).unwrap();
        } else {
            std::fs::create_dir(&outside).unwrap();
            symlink(&outside, &f.library).unwrap();
            std::fs::write(&file, bytes).unwrap();
        }
        assert!(f.action(json!({"action":"status"})).await.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        if !link_settings {
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        }
    }
}

#[test]
fn extraction_requests_cannot_select_an_automatic_model() {
    let request = json!({"action":"resolve","key":key(),"source":{"location":"synthetic.json","automatic":true}});
    assert!(parse_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn local_drafts_reuse_exact_revisions_and_acceptance_survives_restart() {
    let f = Fixture::new();
    f.configure(Value::Null).await;
    let file = f.descriptor("original.json", false);
    let first = f
        .action(json!({"action":"add","key":key(),"file":file}))
        .await
        .unwrap();
    let revision = first["package"]["revision"].as_str().unwrap();
    assert_eq!(first["package"]["accepted"], false);
    let reused = f
        .action(json!({"action":"resolve","key":key(),"source":{"location":"/absent/do-not-read"}}))
        .await
        .unwrap();
    assert_eq!(reused["package"], first["package"]);
    assert_eq!(reused["from"], "local");
    assert!(f.action(json!({"action":"resolve","key":key(),"revision":"0".repeat(64),"source":{"location":"/absent"}})).await.is_err());
    assert!(f.action(json!({"action":"recipe","key":key(),"revision":revision,"environment":"dev","alias":"inventory","endpoint":"http://127.0.0.1:8765"})).await.is_err());
    let accepted = f
        .action(json!({"action":"accept","key":key(),"revision":revision}))
        .await
        .unwrap();
    assert_eq!(accepted["package"]["accepted"], true);
    let recipe=f.action(json!({"action":"recipe","key":key(),"revision":revision,"environment":"dev","alias":"inventory","endpoint":"http://127.0.0.1:8765"})).await.unwrap();
    let parsed: Value = serde_json::from_str(recipe["recipe"].as_str().unwrap()).unwrap();
    assert_eq!(
        parsed["environments"]["dev"]["imports"]["inventory"]["source"]["sha256"],
        revision
    );
    for endpoint in [
        "file:///tmp/local",
        "https://user:secret@example.invalid",
        "https://example.invalid?token=x",
    ] {
        assert!(f.action(json!({"action":"recipe","key":key(),"revision":revision,"environment":"dev","alias":"inventory","endpoint":endpoint})).await.is_err());
    }
    let second = f
        .action(json!({"action":"add","key":key(),"file":f.descriptor("changed.json",true)}))
        .await
        .unwrap();
    assert_ne!(first["package"]["revision"], second["package"]["revision"]);
    let diff=f.action(json!({"action":"compare","key":key(),"before":revision,"after":second["package"]["revision"]})).await.unwrap();
    assert!(!diff["changes"].as_array().unwrap().is_empty());
    let pinned = f
        .action(json!({"action":"resolve","key":key(),"revision":revision}))
        .await
        .unwrap();
    assert_eq!(pinned["descriptor"], first["descriptor"]);
    assert_eq!(pinned["package"]["accepted"], true);
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

fn repository(root: &Path, bytes: &[u8]) -> (PathBuf, String) {
    let repo = root.join("repository");
    std::fs::create_dir(&repo).unwrap();
    let revision = digest(bytes);
    std::fs::write(repo.join("descriptor.json"), bytes).unwrap();
    std::fs::write(repo.join("manifest.json"),serde_json::to_vec(&json!({"version":1,"key":key(),"revision":revision,"descriptor":"descriptor.json","reviewed":true,"source":null})).unwrap()).unwrap();
    std::fs::write(repo.join("catalog.json"),serde_json::to_vec(&json!({"version":1,"packages":[{"key":key(),"revision":revision,"manifest":"manifest.json"}]})).unwrap()).unwrap();
    (repo, revision)
}
#[tokio::test(flavor = "multi_thread")]
async fn repository_materializes_verified_packages_without_writes_and_local_hit_is_offline() {
    let f = Fixture::new();
    let original = include_bytes!("../../../examples/api-import/inventory.json");
    let (repo, revision) = repository(&f.root, original);
    let before: Vec<_> = std::fs::read_dir(&repo)
        .unwrap()
        .map(|p| {
            let p = p.unwrap().path();
            (p.file_name().unwrap().to_owned(), std::fs::read(p).unwrap())
        })
        .collect();
    f.configure(json!({"kind":"local","directory":repo})).await;
    let result = f
        .action(json!({"action":"resolve","key":key()}))
        .await
        .unwrap();
    assert_eq!(result["from"], "repository");
    assert_eq!(result["package"]["accepted"], true);
    for (name, bytes) in before {
        assert_eq!(std::fs::read(repo.join(name)).unwrap(), bytes);
    }
    assert_eq!(std::fs::read_dir(&repo).unwrap().count(), 3);
    std::fs::rename(&repo, f.root.join("offline-repository")).unwrap();
    let reused = f
        .action(json!({"action":"resolve","key":key(),"revision":revision}))
        .await
        .unwrap();
    assert_eq!(reused["from"], "local");
    assert!(f.action(json!({"action":"resolve","key":{"service":"missing","apiVersion":"v1","scope":"default"},"source":{"location":"/not-read"}})).await.unwrap_err().to_string().contains("repository"));
}
#[tokio::test(flavor = "multi_thread")]
async fn tampering_and_concurrent_writers_do_not_fall_back_or_change_existing_content() {
    let f = Fixture::new();
    f.configure(Value::Null).await;
    let first = f
        .action(json!({"action":"add","key":key(),"file":f.descriptor("data.json",false)}))
        .await
        .unwrap();
    let revision = first["package"]["revision"].as_str().unwrap();
    let held = Library::open(&f.library).unwrap();
    assert!(
        f.action(json!({"action":"list"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    drop(held);
    std::fs::write(f.library.join(format!("objects/{revision}.json")), b"{}").unwrap();
    assert!(
        f.action(json!({"action":"resolve","key":key(),"source":{"location":"/not-read"}}))
            .await
            .unwrap_err()
            .to_string()
            .contains("hash mismatch")
    );
    let library = Library::open(&f.library).unwrap();
    assert!(library.find(&native_key(), Some(revision)).is_some());
}
#[tokio::test(flavor = "multi_thread")]
async fn corrupt_repo_manifest_and_catalog_are_errors_not_ingestion_misses() {
    let f = Fixture::new();
    let (repo, _) = repository(
        &f.root,
        include_bytes!("../../../examples/api-import/inventory.json"),
    );
    f.configure(json!({"kind":"local","directory":repo})).await;
    std::fs::write(repo.join("descriptor.json"), b"{}").unwrap();
    assert!(
        f.action(json!({"action":"resolve","key":key(),"source":{"location":"/not-read"}}))
            .await
            .unwrap_err()
            .to_string()
            .contains("SHA-256 mismatch")
    );
    std::fs::write(
        repo.join("catalog.json"),
        b"{\"version\":1,\"version\":1,\"packages\":[]}",
    )
    .unwrap();
    assert!(f.action(json!({"action":"catalog"})).await.is_err());
}
#[tokio::test(flavor = "multi_thread")]
async fn scopes_are_distinct_and_invalid_metadata_cannot_escape_the_repository() {
    let f = Fixture::new();
    let (repo, _) = repository(
        &f.root,
        include_bytes!("../../../examples/api-import/inventory.json"),
    );
    f.configure(json!({"kind":"local","directory":repo})).await;
    let missing=f.action(json!({"action":"resolve","key":{"service":"inventory","apiVersion":"v1","scope":"readonly"}})).await.unwrap();
    assert_eq!(missing["found"], false);
    let mut catalog: Value =
        serde_json::from_slice(&std::fs::read(repo.join("catalog.json")).unwrap()).unwrap();
    catalog["packages"][0]["manifest"] = json!("../outside.json");
    std::fs::write(
        repo.join("catalog.json"),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    assert!(f.action(json!({"action":"catalog"})).await.is_err());
    #[cfg(unix)]
    {
        let outside = f.root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, f.library.join("objects/link")).unwrap();
        let library = Library::open(&f.library).unwrap();
        assert!(library.descriptor("../outside").is_err());
    }
}

async fn documentation_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/documentation", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    (url, task)
}

async fn describe(
    service: &ApiLibrary,
    base: &Path,
    location: String,
    is_url: bool,
    out: Option<String>,
) -> Result<Value, wes_engine::providers::InvocationError> {
    let value = service
        .describe_service(base.into())
        .describe(
            wes_engine::describe::DescribeRequest {
                location,
                is_url,
                provider: "inventory".into(),
                out,
            },
            wes_engine::driver::CancellationToken::new(),
        )
        .await?;
    Ok(serde_json::from_slice(
        &wes_adapters::codec::encode_json(value.data(), Default::default()).unwrap(),
    )
    .unwrap())
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn describe_retains_whole_document_without_import_and_exports_once() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let descriptor = f.draft_answer("answer.json");
    let executable = f.root.join("synthetic-describe");
    let received = f.root.join("received");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n[ \"$#\" = 6 ] || exit 9\n/bin/cat > '{}'\n/bin/cat '{}'\n",
            received.display(),
            descriptor.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    let original = r#"{"openapi":"3.0.3","info":{"description":"Entire original document"}}"#;
    let (url, serving) = documentation_server(original).await;
    let service = ApiLibrary::new(f.home.clone());
    let output = f.root.join("export.json");
    let first = describe(
        &service,
        &f.root,
        url.clone(),
        true,
        Some(output.to_string_lossy().into()),
    )
    .await
    .unwrap();
    serving.await.unwrap();
    assert_eq!(std::fs::read_to_string(received).unwrap(), original);
    let exported: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(exported["source"]["location"], url);
    // Draft publication restores current response maps and preserves native runtime contracts.
    let original_descriptor: Value = serde_json::from_slice(include_bytes!(
        "../../../examples/api-import/inventory.json"
    ))
    .unwrap();
    assert_eq!(exported["types"], original_descriptor["types"]);
    assert_eq!(exported["operations"], original_descriptor["operations"]);
    assert_eq!(first["operationCount"], 3);
    assert!(first.get("descriptor").is_none());
    assert_eq!(first["package"]["accepted"], false);
    let local = f.root.join("openapi.json");
    std::fs::write(&local, original).unwrap();
    let again = describe(
        &service,
        &f.root,
        local.to_string_lossy().into(),
        false,
        Some(output.to_string_lossy().into()),
    )
    .await
    .unwrap_err();
    assert!(
        again.to_string().contains("Draft saved; export failed"),
        "{again}"
    );
    assert!(again.to_string().contains("already exists"), "{again}");
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&output).unwrap()).unwrap(),
        exported
    );
    assert_eq!(
        f.action(json!({"action":"listDrafts"})).await.unwrap()["drafts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn documentation_command_requires_local_configuration_but_ready_descriptors_still_work() {
    use wes_adapters::imports::SpecDocuments;
    let f = Fixture::new();
    for (body, ready) in [
        ("<html>API documentation</html>", false),
        (
            include_str!("../../../tests/fixtures/catalog.provider.json"),
            true,
        ),
    ] {
        let (url, serving) = documentation_server(body).await;
        let service = ApiLibrary::new(f.home.clone());
        let result = tokio::task::spawn_blocking(move || service.capture(&url, 1024 * 1024))
            .await
            .unwrap();
        if ready {
            assert_eq!(result.unwrap().format(), "spec/json/v1");
        } else {
            assert!(result.unwrap_err().to_string().contains(":describe"));
        }
        serving.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn source_edits_use_revision_checks_and_preserve_old_bytes() {
    let f = Fixture::new();
    f.configure(Value::Null).await;
    let descriptor = f.descriptor("edit.json", false);
    let original = std::fs::read(&descriptor).unwrap();
    let first = Library::open(&f.library)
        .unwrap()
        .save(native_key(), &original, "fixture".into(), None, false)
        .unwrap();
    let invalid =
        json!({"action":"saveDescriptor","key":key(),"revision":first.revision,"text":"{}"});
    assert!(f.action(invalid).await.is_err());
    assert_eq!(
        f.action(json!({"action":"validateDescriptor","text":"{}"}))
            .await
            .unwrap()["valid"],
        false
    );
    let changed = std::fs::read_to_string(f.descriptor("changed.json", true)).unwrap();
    let request =
        json!({"action":"saveDescriptor","key":key(),"revision":first.revision,"text":changed});
    let second = f.action(request.clone()).await.unwrap();
    assert_eq!(
        second["descriptor"]["source"]["provenance"]["status"],
        "stale"
    );
    assert_ne!(second["package"]["revision"], json!(first.revision));
    assert!(f.action(request).await.is_err());
    let old = f
        .action(json!({"action":"inspect","key":key(),"revision":first.revision}))
        .await
        .unwrap();
    assert_eq!(old["source"], String::from_utf8(original).unwrap());
    assert_eq!(
        old["descriptor"]["source"]["provenance"]["status"],
        "current"
    );
    assert_eq!(
        old["descriptor"]["source"]["provenance"]["entries"],
        second["descriptor"]["source"]["provenance"]["entries"]
    );
    let output = f.root.join("saved.json");
    let export =
        json!({"action":"exportDescriptor","key":key(),"revision":first.revision,"file":output});
    f.action(export.clone()).await.unwrap();
    assert!(f.action(export).await.is_err());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_owned_describe_process_saves_nothing_and_joins_child() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let executable = f.root.join("slow");
    let marker = f.root.join("pid");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\necho $$ > '{}'\n/bin/sleep 60\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    let source = f.root.join("source.json");
    std::fs::write(&source, r#"{"openapi":"3.0.3"}"#).unwrap();
    let token = wes_engine::driver::CancellationToken::new();
    let service = ApiLibrary::new(f.home.clone()).describe_service(f.root.clone());
    let future = service.describe(
        wes_engine::describe::DescribeRequest {
            location: source.to_string_lossy().into(),
            is_url: false,
            provider: "inventory".into(),
            out: None,
        },
        token.clone(),
    );
    let job = tokio::spawn(future);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap(),
        Err(wes_engine::providers::InvocationError::Cancelled)
    ));
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn early_extractor_rejection_preserves_safe_error_category_with_large_stdin() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let executable = f.root.join("reject");
    std::fs::write(
        &executable,
        "#!/bin/sh\necho synthetic-secret >&2\nexit 21\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    let source = f.root.join("large.json");
    std::fs::write(
        &source,
        format!(
            r#"{{"openapi":"3.0.3","description":"{}"}}"#,
            "x".repeat(256 * 1024)
        ),
    )
    .unwrap();
    let service = ApiLibrary::new(f.home.clone());
    let error = describe(
        &service,
        &f.root,
        source.to_string_lossy().into(),
        false,
        None,
    )
    .await
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("unsupported or invalid"), "{text}");
    assert!(!text.contains("synthetic-secret"));
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn incomplete_describe_survives_editing_validation_review_and_export() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let answer: Value = serde_json::from_slice(include_bytes!(
        "../../../examples/editable-api-draft/incomplete-draft.json"
    ))
    .unwrap();
    let envelope = json!({"draft":answer,"source":{"sha256":"synthetic","format":"synthetic-editable-draft","provenance":{"version":1,"status":"current","entries":[]}}});
    let file = f.root.join("draft.json");
    std::fs::write(&file, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let extractor = f.root.join("extractor");
    std::fs::write(
        &extractor,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\n/bin/cat '{}'\n",
            file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&extractor, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(
        json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":extractor}}),
    )
    .await
    .unwrap();
    let doc = f.root.join("openapi.json");
    std::fs::write(&doc, br#"{"openapi":"3.0.3"}"#).unwrap();
    let export = f.root.join("spec.json");
    let result = describe(
        &ApiLibrary::new(f.home.clone()),
        &f.root,
        doc.to_string_lossy().into(),
        false,
        Some(export.to_string_lossy().into()),
    )
    .await
    .unwrap_err();
    assert!(
        result
            .to_string()
            .contains("Draft saved; export was not performed")
    );
    assert!(!export.exists());
    assert_eq!(
        f.action(json!({"action":"list"})).await.unwrap()["packages"],
        json!([])
    );
    let drafts = f.action(json!({"action":"listDrafts"})).await.unwrap();
    let saved = &drafts["drafts"][0];
    let key = saved["key"].clone();
    let rev = saved["revision"].clone();
    let inspected = f
        .action(json!({"action":"inspectDraft","key":key,"revision":rev}))
        .await
        .unwrap();
    assert_eq!(inspected["validation"]["valid"], false);
    assert!(
        f.action(json!({"action":"reviewDraft","key":key,"revision":rev}))
            .await
            .is_err()
    );
    assert!(
        f.action(json!({"action":"exportDraft","key":key,"revision":rev,"file":export}))
            .await
            .is_err()
    );
    let broken = f
        .action(json!({"action":"saveDraft","key":key,"revision":rev,"text":"{\n"}))
        .await
        .unwrap();
    assert_eq!(broken["text"], "{\n");
    let saved=f.action(json!({"action":"saveDraft","key":key,"revision":broken["draft"]["revision"],"text":include_str!("../../../examples/editable-api-draft/completed-draft.json")})).await.unwrap();
    assert_eq!(saved["validation"]["valid"], true);
    assert_eq!(saved["draft"]["accepted"], false);
    assert!(saved["descriptorPath"].is_string());
    // Valid but not reviewed is exportable/importable; review is not an authority gate.
    f.action(json!({"action":"exportDraft","key":key,"revision":saved["draft"]["revision"],"file":export})).await.unwrap();
    wes_adapters::api_library::validate_descriptor(&std::fs::read(&export).unwrap()).unwrap();
    assert!(f.action(json!({"action":"exportDraft","key":key,"revision":saved["draft"]["revision"],"file":export})).await.is_err());
    let reviewed = f
        .action(json!({"action":"reviewDraft","key":key,"revision":saved["draft"]["revision"]}))
        .await
        .unwrap();
    assert_eq!(reviewed["draft"]["accepted"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn artifact_list_returns_both_catalogs_and_validation_never_contends_with_storage() {
    let f = Fixture::new();
    let ready = include_str!("../../../examples/editable-api-draft/completed-draft.json");
    let descriptor = include_str!("../../../examples/api-import/inventory.json");
    for (action, text) in [("validateDraft", ready), ("validateDescriptor", descriptor)] {
        assert_eq!(
            f.action(json!({"action":action,"text":text}))
                .await
                .unwrap()["valid"],
            true
        );
    }
    assert!(
        !f.home.exists(),
        "Pure validation must not create a data directory or lock"
    );
    f.configure(Value::Null).await;
    let draft = Library::open(&f.library)
        .unwrap()
        .create_draft(
            native_key(),
            ready.into(),
            json!({}),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let snapshot = f.action(json!({"action":"list"})).await.unwrap();
    assert_eq!(snapshot["drafts"][0], draft["draft"]);
    assert_eq!(
        snapshot["packages"][0]["revision"],
        draft["draft"]["descriptorRevision"]
    );
    let _writer = wes_adapters::api_library::exclusive_lock(&f.home, ".api-library.lock").unwrap();
    for (action, text) in [("validateDraft", ready), ("validateDescriptor", descriptor)] {
        assert_eq!(
            f.action(json!({"action":action,"text":text}))
                .await
                .unwrap()["valid"],
            true
        );
    }
    assert!(
        f.action(json!({"action":"list"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    assert!(f.action(json!({"action":"saveDraft","key":native_key(),"revision":draft["draft"]["revision"],"text":"{"})).await.unwrap_err().to_string().contains("busy"));
}

#[tokio::test]
async fn recipe_auth_selection_uses_shared_slot_resolution_without_secret_values() {
    let f = Fixture::new();
    f.configure(Value::Null).await;
    let path = f.root.join("auth.json");
    std::fs::write(
        &path,
        include_bytes!("../../../examples/http-auth-alternatives/service.json"),
    )
    .unwrap();
    let added = f
        .action(json!({"action":"add","key":key(),"file":path}))
        .await
        .unwrap();
    let revision = added["package"]["revision"].clone();
    f.action(json!({"action":"accept","key":key(),"revision":revision}))
        .await
        .unwrap();
    for (choices, credentials) in [
        (
            json!(["apiKey", "apiSecret"]),
            json!({"apiKey":"qa/key","apiSecret":"qa/secret"}),
        ),
        (
            json!(["basic"]),
            json!({"basic.username":"qa/user","basic.password":"qa/password"}),
        ),
    ] {
        let req = json!({"action":"recipe","key":key(),"revision":revision,"environment":"qa","alias":"demo","endpoint":"http://127.0.0.1:1","auth":{"listItems":choices},"credentials":credentials});
        let result = f.action(req.clone()).await.unwrap();
        let recipe: Value = serde_json::from_str(result["recipe"].as_str().unwrap()).unwrap();
        assert_eq!(
            recipe["environments"]["qa"]["imports"]["demo"]["bind"]["auth"]["listItems"],
            choices
        );
        let mut bad = req.clone();
        bad["credentials"] = json!({});
        assert!(f.action(bad).await.is_err());
        let mut bad = req;
        bad["auth"]["listItems"] = json!(["wrong"]);
        assert!(f.action(bad).await.is_err());
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ingested_descriptor_exports_portable_location_but_keeps_local_origin_and_source() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let answer = f.descriptor("answer.json", false);
    let executable = f.root.join("extract");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\n/bin/cat '{}'\n",
            answer.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.action(json!({"action":"configure","settings":{"localDirectory":f.library,"extractor":executable}})).await.unwrap();
    let source = f.root.join("openapi.json");
    let original =
        br#"{"openapi":"3.0.3","info":{"title":"Synthetic API","version":"1"},"paths":{}}"#;
    std::fs::write(&source, original).unwrap();
    let ingested = f
        .action(json!({"action":"ingest","key":key(),"source":{"location":source}}))
        .await
        .unwrap();
    assert_eq!(ingested["descriptor"]["source"]["location"], "openapi.json");
    assert!(
        ingested["package"]["origin"]
            .as_str()
            .unwrap()
            .contains(&source.to_string_lossy().to_string())
    );
    assert_eq!(ingested["package"]["sourceDigest"], digest(original));
    let output = f.root.join("shared.json");
    f.action(json!({"action":"exportDescriptor","key":key(),"revision":ingested["package"]["revision"],"file":output})).await.unwrap();
    let exported = std::fs::read_to_string(output).unwrap();
    assert!(!exported.contains(&f.root.to_string_lossy().to_string()));
    let before: Value = serde_json::from_slice(&std::fs::read(answer).unwrap()).unwrap();
    let after: Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(
        after["source"]["provenance"],
        before["source"]["provenance"]
    );
}
