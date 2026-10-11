use super::*;
#[path = "hardening.rs"]
mod hardening;
use wes_adapters::{
    datasets::{DatasetStore, StoreLimits},
    journal::{Durability as HistoryDurability, FileHistory, ReadLimits},
    process::{self, ProcessConfig, ProcessInvoker},
};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    history::HistoryCaptureLimits,
    recording::{RecorderLimits, spawn_recorder},
    storage::{AcquiredFuture, AcquisitionContext, StoreError, spawn_storage},
};
struct CountProcess {
    inner: ProcessInvoker,
    count: Arc<AtomicUsize>,
}

#[tokio::test]
async fn declared_text_and_bytes_use_bounded_acquisition_with_their_original_public_types() {
    use wes_engine::imports::{ImportMode, ImportRequest, ImportSnapshot, Importer};
    let root = tempfile::tempdir().unwrap();
    let program = finite_fixture(root.path()).await;
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (mut base, _) = workspace(Arc::new(Definitions::default()));
    for (name, format, ty) in [
        ("plain_text", "text", "Text"),
        ("plain_bytes", "bytes", "Bytes"),
    ] {
        let doc = serde_json::json!({"version":1,"provider":name,"backend":{"kind":"process","bin":program},"operations":[{"path":["run"],"argv":["finite","","","131072"],"result":{"format":format,"type":ty}}]});
        let file = format!("{name}.yaml");
        std::fs::write(root.path().join(&file), doc.to_string()).unwrap();
        let importer = wes_adapters::provider_definition::ProviderImporter::new(
            root.path(),
            ProcessConfig {
                output_bytes: 4096,
                ..Default::default()
            },
        )
        .unwrap();
        let request = ImportRequest::new(
            "provider".into(),
            None,
            [(
                "file".into(),
                wes_core::Value::new(
                    Shape::Primitive(Primitive::Text),
                    Data::Text(file.into()),
                    Default::default(),
                )
                .unwrap(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let recipe = importer.capture(&request, 64 * 1024).unwrap();
        let product = importer
            .build(&ImportSnapshot::new(request, recipe), ImportMode::Live)
            .unwrap();
        base.register_provider(product.description().clone(), product.invoker().clone())
            .unwrap();
    }
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let made = submit(
        &handle,
        "typed",
        ":transient {\nplain_text run > text\nplain_bytes run > bytes\n} > typed",
    )
    .await;
    assert!(
        made.diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{:?}",
        made.diagnostics
    );
    let text = read(&handle, "typed.text").await;
    let bytes = read(&handle, "typed.bytes").await;
    assert!(matches!(text,Data::Text(s) if s.len()==131072 && s.ends_with('\n')));
    assert!(matches!(bytes,Data::Bytes(b) if b.len()==131072 && b.last()==Some(&b'\n')));
    assert_eq!(files(&root.path().join("archive")), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}
impl Invoker for CountProcess {
    fn invoke(&self, call: Call, cancel: CancellationToken) -> InvocationFuture {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.inner.invoke(call, cancel)
    }
    fn invoke_acquired(
        &self,
        call: Call,
        cancel: CancellationToken,
        context: AcquisitionContext,
    ) -> AcquiredFuture {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.inner.invoke_acquired(call, cancel, context)
    }
}
async fn finite_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let executable = root.join(if cfg!(windows) {
        "finite.exe"
    } else {
        "finite"
    });
    let target = executable.clone();
    tokio::task::spawn_blocking(move || {
        let rustc = std::path::Path::new(env!("CARGO")).with_file_name(if cfg!(windows) {
            "rustc.exe"
        } else {
            "rustc"
        });
        let result = std::process::Command::new(rustc)
            .arg("--edition=2024")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/support/process_fixture.rs"),
            )
            .arg("-o")
            .arg(target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    })
    .await
    .unwrap();
    executable
}
fn large_workspace(
    definitions: Arc<Definitions>,
    program: &std::path::Path,
) -> (Workspace, Arc<AtomicUsize>) {
    let (mut base, _) = workspace(definitions);
    let count = Arc::new(AtomicUsize::new(0));
    let (description, inner) = process::provider(
        "finite",
        vec![
            program.to_str().unwrap().into(),
            "finite".into(),
            "".into(),
            "".into(),
        ],
        "arg",
        ProcessConfig {
            timeout: std::time::Duration::from_secs(180),
            output_bytes: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    base.register_provider(
        description,
        Arc::new(CountProcess {
            inner,
            count: count.clone(),
        }),
    )
    .unwrap();
    (base, count)
}
#[tokio::test]
async fn finite_process_500_mib_keeps_checkpoint_and_old_fields_without_replaying_its_provider() {
    bounded_pipeline(
        500 * 1024 * 1024,
        StoreWorkerLimits::default(),
        64 * 1024 * 1024,
    )
    .await;
}
#[tokio::test]
async fn selected_cold_saved_fields_and_checkpoint_resume_fit_a_small_resident_budget() {
    bounded_pipeline(
        8 * 1024 * 1024,
        StoreWorkerLimits::default(),
        4 * 1024 * 1024,
    )
    .await;
}
#[tokio::test]
async fn checkpoint_validation_and_dataset_transaction_share_serial_scratch_credit() {
    use wes_engine::storage::datasets::DatasetStorage;
    let root = tempfile::tempdir().unwrap();
    let datasets = DatasetStore::open(
        &root.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let dataset_scratch = datasets.read_charge();
    drop(datasets);
    let source_bytes = 8 * 1024 * 1024u64;
    let validation_scratch = wes_budgets::get("storage.acquisition.metadata.bytes") * 16
        + 2 * wes_engine::storage::CHUNK_BYTES as u64;
    let credit = dataset_scratch.max(validation_scratch) + 2 * 1024 * 1024;
    assert!(credit < dataset_scratch + validation_scratch);
    bounded_pipeline(
        source_bytes,
        StoreWorkerLimits {
            bytes: std::num::NonZeroU32::new(u32::try_from(credit).unwrap()).unwrap(),
            ..StoreWorkerLimits::default()
        },
        4 * 1024 * 1024,
    )
    .await;
}
async fn bounded_pipeline(bytes: u64, worker_limits: StoreWorkerLimits, resident: u64) {
    let root = tempfile::tempdir().unwrap();
    let program = finite_fixture(root.path()).await;
    let definitions = Arc::new(Definitions::default());
    let storage = || {
        spawn_storage(
            TieredValues::open(
                &root.path().join("live"),
                &root.path().join("archive"),
                Limits::default(),
                Durability::FileAndDirectory,
                None,
            )
            .unwrap(),
            DatasetStore::open(
                &root.path().join("datasets"),
                Durability::FileAndDirectory,
                StoreLimits::default(),
            )
            .unwrap(),
            worker_limits,
        )
        .map(|(worker, task)| {
            (
                worker.with_resident_memory(wes_engine::storage::ResidentMemory::new(
                    resident, 10000,
                )),
                task,
            )
        })
        .unwrap()
    };
    let (worker, storage_task) = storage();
    let (recorder, writer) = spawn_recorder(
        FileHistory::open(
            &root.path().join("history"),
            ReadLimits::default(),
            HistoryDurability::FileAndDirectory,
        )
        .unwrap(),
        RecorderLimits::default(),
    )
    .unwrap();
    let (base, calls) = large_workspace(definitions.clone(), &program);
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let types=submit(&handle,"types",r#":package load source:"types: {Step: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def count(state:Int, context:Int, item:Unknown) -> Step as :calc pure { return {state:state+1,outputs:[state+1]}; }"#).await;
    assert!(
        types
            .diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{:?}",
        types.diagnostics
    );
    let source = format!(
        ":transient {{\nfinite run arg:[\"{bytes}\"] > raw\n:calc pure {{ return $raw.exitCode; }} > code\n:scan source:$raw.stdout transition:count initial:0 context:0 profile:LinesUtf8 sink:dataset records:3 > analysis\n}} > study"
    );
    let made = submit(&handle, "A", &source).await;
    assert!(
        made.diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{:?}",
        made.diagnostics
    );
    let exported = handle.read_sandbox_export("study", true).await.unwrap();
    let scope = exported.scope.unwrap();
    let (child, scoped) = handle.resolve_read_scope(&scope).await.unwrap();
    let snapshot = child.snapshot().await.unwrap();
    let raw = &snapshot.names["raw"].node;
    assert!(!snapshot.execution.values.contains_key(raw));
    assert!(
        snapshot.execution.stored_values.contains_key(raw),
        "{:?}",
        snapshot.execution.errors
    );
    assert_eq!(read(&handle, "study.raw.exitCode").await, Data::Int(0));
    assert_eq!(read(&handle, "study.code").await, Data::Int(0));
    for (n, body) in [
        "if(false){return $raw.stdout;} return $raw.exitCode;",
        "const unused=()=> $raw.stdout; return $raw.exitCode;",
        "const ignored=false && length($raw.stdout)>0; return $raw.exitCode;",
        "const ignored=()=> $raw.stdout; return 17;",
    ]
    .into_iter()
    .enumerate()
    {
        let made = submit(
            &child,
            &format!("unreachable-{n}"),
            &format!(":calc pure {{ {body} }} > unreachable_{n}"),
        )
        .await;
        assert!(!made.nodes.is_empty(), "{:?}", made.diagnostics);
        child.wait_idle().await.unwrap();
        let got = child.snapshot().await.unwrap();
        let value =
            got.execution.values.get(&made.nodes[0]).unwrap_or_else(|| {
                panic!("unreachable field preparation: {:?}", got.execution.errors)
            });
        assert_eq!(value.data(), &Data::Int(if n == 3 { 17 } else { 0 }));
        assert_eq!(
            value.provenance().policy().origins(),
            snapshot.execution.stored_values[raw]
                .snapshot
                .provenance
                .policy()
                .origins()
        );
    }
    let viewed = submit(&child, "cold-view", ":view create Metric > chart").await;
    assert!(!viewed.nodes.is_empty(), "{:?}", viewed.diagnostics);
    child.wait_idle().await.unwrap();
    let with_view = child.snapshot().await.unwrap();
    assert!(
        with_view.execution.values.contains_key(&viewed.nodes[0]),
        "{:?}",
        with_view.execution.errors
    );
    assert!(child.view_frame(viewed.nodes[0].clone()).await.is_ok());
    submit(
        &child,
        "cold-view-input",
        ":calc pure { return {view:\"metric\",value:0}; } > metric_input",
    )
    .await;
    child.wait_idle().await.unwrap();
    let bound = submit(
        &child,
        "cold-view-field",
        ":view bind $chart input:$metric_input revision:$raw.exitCode > bound_chart",
    )
    .await;
    assert!(!bound.nodes.is_empty(), "{:?}", bound.diagnostics);
    child.wait_idle().await.unwrap();
    let with_view = child.snapshot().await.unwrap();
    assert!(
        with_view.execution.values.contains_key(&bound.nodes[0]),
        "{:?}",
        with_view.execution.errors
    );
    assert_eq!(
        child
            .view_frame(viewed.nodes[0].clone())
            .await
            .unwrap()
            .instances[0]
            .revision,
        1
    );
    let refused = handle
        .read_sandbox_export("study.raw", false)
        .await
        .unwrap_err();
    assert_eq!(
        refused.read_failure_kind(),
        Some(wes_engine::storage::ReadFailureKind::Capacity)
    );
    let source = snapshot.execution.stored_values[raw].clone();
    let mut stdout = source.snapshot.selection.clone();
    stdout.push("stdout".into());
    let stdout = scoped
        .describe(source.snapshot.handle.clone(), stdout)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scoped
            .source_range(stdout.clone(), bytes - 4, 10)
            .await
            .unwrap()
            .bytes,
        b"xxx\n"
    );
    assert!(matches!(
        scoped.materialize(stdout).await,
        Err(StoreError::Limit(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(files(&root.path().join("archive")), 0);
    let analysis = &snapshot.names["analysis"].node;
    let evidence = snapshot
        .execution
        .evidence_values
        .get(analysis)
        .unwrap_or_else(|| panic!("scan failure: {:?}", snapshot.execution.errors));
    let Data::Record(evidence) = evidence.value.data() else {
        panic!(
            "expected incomplete analysis: {:?}",
            snapshot.execution.errors
        );
    };
    assert_eq!(evidence["state"], Data::Int(3));
    let Data::Dataset(prefix) = &evidence["outputs"] else {
        panic!("dataset");
    };
    let prefix = prefix.as_ref().clone();
    let checkpoint = scoped
        .dataset_checkpoint(prefix.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.source.selection, ["stdout"]);
    assert_eq!(checkpoint.next_position, 3 * 65536);
    for (id, command) in [
        ("keep-raw", ":keep $study.raw > saved_raw"),
        ("keep-analysis", ":keep $study.analysis > saved_partial"),
        ("keep-whole", ":keep $study > saved_whole"),
    ] {
        let saved = submit(&handle, id, command).await;
        assert!(
            saved
                .diagnostics
                .diagnostics
                .iter()
                .all(|d| d.severity != wes_language::Severity::Error),
            "{id}: {:?}",
            saved.diagnostics
        );
    }
    let Data::Record(saved) = read(&handle, "saved_whole").await else {
        panic!("saved membership");
    };
    let Data::List(members) = &saved["members"] else {
        panic!("saved members");
    };
    let member = |name: &str| {
        members
            .iter()
            .find_map(|row| match row {
                Data::Record(fields) if fields["name"] == Data::Text(name.into()) => Some(fields),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing saved member {name}"))
    };
    assert_eq!(member("raw")["retainedReference"], Data::Bool(true));
    assert_eq!(
        member("raw")["availability"],
        Data::Text("stored; decoded capacity required".into())
    );
    assert!(!member("raw").contains_key("value"));
    assert_eq!(member("code")["value"], Data::Int(0));
    let refused = handle
        .read_sandbox_export("saved_raw", false)
        .await
        .unwrap_err();
    assert_eq!(
        refused.read_failure_kind(),
        Some(wes_engine::storage::ReadFailureKind::Capacity)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        read(&handle, "saved_whole.raw.exitCode").await,
        Data::Int(0)
    );
    let use_saved = submit(
        &handle,
        "use-saved",
        ":calc pure { return $saved_whole.raw.exitCode + $saved_whole.code; } > saved_code",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let live = handle.snapshot().await.unwrap();
    assert_eq!(
        live.execution
            .values
            .get(&use_saved.nodes[0])
            .unwrap_or_else(|| panic!(
                "saved calculation: {:?}; diagnostics: {:?}",
                live.execution.errors, use_saved.diagnostics
            ))
            .data(),
        &Data::Int(0)
    );
    for (n, body) in [
        "return $raw;",
        "const root=$raw; return $raw.exitCode;",
        "const key='stdout'; return $raw[key];",
    ]
    .into_iter()
    .enumerate()
    {
        let refused = submit(
            &child,
            &format!("whole-{n}"),
            &format!(":calc pure {{ {body} }} > too_large_{n}"),
        )
        .await;
        assert!(!refused.nodes.is_empty(), "{:?}", refused.diagnostics);
        child.wait_idle().await.unwrap();
        let after = child.snapshot().await.unwrap();
        assert_eq!(after.execution.errors[&refused.nodes[0]].code(), "RUN007");
        assert_eq!(
            after.execution.stored_values[raw].run,
            snapshot.execution.stored_values[raw].run
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    assert_eq!(read(&handle, "study.raw.exitCode").await, Data::Int(0));
    submit(
        &handle,
        "B",
        ":transient { finite run arg:[\"10\"] > raw } > study",
    )
    .await;
    assert_eq!(read(&handle, "study.raw.exitCode").await, Data::Int(0));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(read(&handle, "saved_raw.exitCode").await, Data::Int(0));
    recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "bounded::saved_process_source_reopens_in_a_fresh_process",
            "--nocapture",
        ])
        .env_clear()
        .env("WES_BOUNDED_REOPEN_ROOT", root.path())
        .env(
            "WES_BOUNDED_REOPEN_WORK_BYTES",
            worker_limits.bytes.get().to_string(),
        )
        .env("WES_BOUNDED_REOPEN_RESIDENT", resident.to_string())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "saved source must reopen in an independent process"
    );
}

#[tokio::test]
async fn saved_process_source_reopens_in_a_fresh_process() {
    let Some(root) = std::env::var_os("WES_BOUNDED_REOPEN_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let program = root.join(if cfg!(windows) {
        "finite.exe"
    } else {
        "finite"
    });
    let resident = std::env::var("WES_BOUNDED_REOPEN_RESIDENT")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let worker_limits = StoreWorkerLimits {
        bytes: std::num::NonZeroU32::new(
            std::env::var("WES_BOUNDED_REOPEN_WORK_BYTES")
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap(),
        ..StoreWorkerLimits::default()
    };
    let (worker, storage_task) = spawn_storage(
        TieredValues::open(
            &root.join("live"),
            &root.join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        DatasetStore::open(
            &root.join("datasets"),
            Durability::FileAndDirectory,
            StoreLimits::default(),
        )
        .unwrap(),
        worker_limits,
    )
    .unwrap();
    let worker =
        worker.with_resident_memory(wes_engine::storage::ResidentMemory::new(resident, 10000));
    let mut history = FileHistory::open(
        &root.join("history"),
        ReadLimits::default(),
        HistoryDurability::FileAndDirectory,
    )
    .unwrap();
    let image = history.capture(HistoryCaptureLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let (base, calls) = large_workspace(Arc::new(Definitions::default()), &program);
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        restored.report().problems.is_empty(),
        "{:?}",
        restored.report()
    );
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert_eq!(
        read(&handle, "saved_whole.raw.exitCode").await,
        Data::Int(0)
    );
    assert_eq!(read(&handle, "saved_whole.code").await, Data::Int(0));
    let continued=submit(&handle,"resume",":transient {\n:scan excerpt $saved_partial from:\"65530\" limit:12 > excerpt\n:scan resume $saved_partial > continued\n} > next").await;
    assert!(
        continued
            .diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{:?}",
        continued.diagnostics
    );
    let next = handle.read_sandbox_export("next", true).await.unwrap();
    let (child, _) = handle
        .resolve_read_scope(next.scope.as_ref().unwrap())
        .await
        .unwrap();
    let next = child.snapshot().await.unwrap();
    assert!(
        next.execution
            .evidence_values
            .contains_key(&next.names["continued"].node),
        "{:?}",
        next.execution.errors
    );
    let excerpt = read(&handle, "next.excerpt").await;
    let Data::Record(excerpt) = excerpt else {
        panic!("excerpt");
    };
    assert_eq!(
        excerpt["data"],
        Data::Bytes(b"xxxxx\nxxxxxx".as_slice().into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}
