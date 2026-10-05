use super::*;
use wes_engine::{
    imports::{ImportCapture, ImportMode, Importers},
    workspace::{Preparation, ReplayWorkspace},
};
#[path = "../support/imports.rs"]
mod support;
use support::Fixture;

fn registered() -> (Workspace, Arc<Fixture>) {
    let (mut workspace, _) = workspace();
    let fixture = Arc::new(Fixture::default());
    workspace
        .register_importer("fixture".into(), fixture.clone())
        .unwrap();
    (workspace, fixture)
}
fn statement(text: &str) -> wes_language::Statement {
    wes_language::parse(&SourceText::new("test", text))
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
}
fn meta(workspace: &Workspace, text: &str) -> wes_engine::workspace::PreparedMeta {
    let Preparation::Meta(meta) = workspace.prepare(&statement(text)).unwrap() else {
        panic!("meta")
    };
    meta
}
fn value<'a>(workspace: &'a Workspace, name: &str) -> &'a Data {
    workspace
        .runtime()
        .value_of(&workspace.resolve(name).unwrap().node)
        .unwrap()
        .data()
}

#[tokio::test]
async fn imported_calls_bind_in_source_order_but_install_and_execution_are_explicit() {
    let (mut workspace, fixture) = registered();
    let text = ":import fixture file:first as:library\nlibrary get > first\n:import fixture file:second as:library replace:true\nlibrary get > second";
    let prepared = plan(&workspace, text, no_files()).await;
    assert_eq!(prepared.accepted().len(), 4);
    assert_eq!(prepared.record().unwrap().imports.len(), 2);
    assert!(
        prepared
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.code == "IMP006" && d.severity == Severity::Warning)
    );
    assert!(workspace.catalogue().provider("library").is_none());
    assert!(workspace.runtime().graph().is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert!(workspace.catalogue().provider("library").is_some());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    run_all(&mut workspace).await;
    assert_eq!(value(&workspace, "first"), &Data::Text("first".into()));
    assert_eq!(value(&workspace, "second"), &Data::Text("second".into()));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn accepted_only_import_evidence_is_cached_and_reconstructed_without_live_reads() {
    let (workspace, fixture) = registered();
    let text = ":import fixture file:bad as:broken\n:import fixture file:good as:library\n:import fixture file:good as:library replace:true\nlibrary get > output";
    let prepared = plan(&workspace, text, no_files()).await;
    assert_eq!(prepared.accepted().len(), 3);
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 2);
    let command = prepared.record().unwrap().clone();
    assert_eq!(command.imports.len(), 1);
    assert!(!command.replay.contains("broken"));
    assert!(command.text.contains("broken"));
    assert_eq!(command.imports[0].request().alias(), Some("library"));
    assert_eq!(command.imports[0].recipe().source(), "good");
    let (base, replay_fixture) = registered();
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let rebuilt = replay
        .prepare(&command, CancellationToken::new())
        .await
        .unwrap();
    assert!(replay.workspace().catalogue().provider("library").is_none());
    assert!(replay.apply(rebuilt).unwrap().effects.is_empty());
    let mut restored = replay.finish();
    assert!(restored.catalogue().provider("library").is_some());
    assert_eq!(restored.resolve("output").unwrap().node, command.nodes[0]);
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(replay_fixture.captures.load(Ordering::SeqCst), 0);
    assert_eq!(replay_fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(*replay_fixture.modes.lock().unwrap(), [ImportMode::Replay]);
    // A new, explicitly submitted call uses the reconstructed provider; old nodes remain held.
    plan(&restored, "library get > fresh", no_files())
        .await
        .commit(&mut restored, Duration::ZERO)
        .unwrap();
    run_all(&mut restored).await;
    assert_eq!(replay_fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(value(&restored, "fresh"), &Data::Text("good".into()));
}

#[tokio::test]
async fn invalid_imports_and_preflight_rejections_do_not_read_or_register() {
    let (mut workspace, fixture) = registered();
    for text in [
        ":import unknown file:good",
        ":import fixture file:$missing",
        ":import fixture as:\"\" file:good",
    ] {
        let prepared = plan(&workspace, text, no_files()).await;
        assert!(prepared.accepted().is_empty(), "{text}");
        assert!(prepared.record().is_none());
    }
    for text in [
        ":import fixture file:good\n:wait $missing",
        ":import fixture file:good\ncatalog echo value:\"unclosed",
    ] {
        assert!(matches!(
            prepare_declarations(
                input(text),
                workspace.draft().unwrap(),
                no_files(),
                CancellationToken::new()
            )
            .await
            .unwrap(),
            SourcePreparation::Rejected { .. }
        ));
    }
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    let prepared = plan(
        &workspace,
        ":def library as catalog echo value:?value\n:import fixture file:good as:library",
        no_files(),
    )
    .await;
    assert_eq!(prepared.accepted().len(), 1);
    assert!(prepared.record().unwrap().imports.is_empty());
    assert!(
        prepared
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.code == "TMP002")
    );
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert!(workspace.catalogue().provider("library").is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let reserved = plan(&workspace, ":import fixture file:good as:list", no_files()).await;
    assert_eq!(reserved.accepted().len(), 1);
    assert!(
        reserved
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.code == "IMP005")
    );
}

#[tokio::test]
async fn captured_import_requires_matching_request_importer_and_structural_revision() {
    let (mut workspace, fixture) = registered();
    let mut registry = Importers::default();
    registry
        .register("fixture".into(), fixture.clone())
        .unwrap();
    let mut capture = ImportCapture::live(registry);
    let text = ":import fixture file:good as:library";
    let request = meta(&workspace, text).import_request().unwrap();
    let captured = capture
        .read(&request, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        workspace
            .prepare_import(
                meta(&workspace, ":import fixture file:other as:library"),
                &captured
            )
            .is_err()
    );
    let prepared_meta = meta(&workspace, text);
    let batch = plan(&workspace, text, no_files()).await;
    workspace
        .register_importer("fixture".into(), Arc::new(Fixture::default()))
        .unwrap();
    assert!(workspace.prepare_import(prepared_meta, &captured).is_err());
    assert!(
        workspace
            .prepare_import(meta(&workspace, text), &captured)
            .is_err()
    );
    assert!(batch.commit(&mut workspace, Duration::ZERO).is_err());
    assert!(workspace.catalogue().provider("library").is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replay_rejects_missing_wrong_or_unusable_import_evidence_atomically() {
    use wes_engine::imports::{ImportRecipe, ImportRequest, ImportSnapshot};
    let (workspace, _) = registered();
    let prepared = plan(
        &workspace,
        ":import fixture file:good as:library\nlibrary get > output",
        no_files(),
    )
    .await;
    let command = prepared.record().unwrap().clone();
    for case in 0..4 {
        let mut bad = command.clone();
        match case {
            0 => bad.imports.clear(),
            1 => {
                bad.imports[0] = ImportSnapshot::new(
                    command.imports[0].request().clone(),
                    ImportRecipe::new("future/v2".into(), "good".into()).unwrap(),
                )
            }
            2 => {
                bad.imports[0] = ImportSnapshot::new(
                    ImportRequest::new(
                        "fixture".into(),
                        Some("other".into()),
                        command.imports[0].request().arguments().clone(),
                    )
                    .unwrap(),
                    command.imports[0].recipe().clone(),
                )
            }
            _ => {
                bad.imports[0] = ImportSnapshot::new(
                    command.imports[0].request().clone(),
                    ImportRecipe::new("fixture/v1".into(), "bad".into()).unwrap(),
                )
            }
        }
        let (base, fixture) = registered();
        let mut replay = ReplayWorkspace::new(base).unwrap();
        assert!(
            replay
                .prepare(&bad, CancellationToken::new())
                .await
                .is_err()
        );
        assert!(replay.workspace().catalogue().provider("library").is_none());
        assert!(replay.workspace().runtime().graph().is_empty());
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn importer_names_are_queryable_without_reading_inputs() {
    let (mut workspace, fixture) = registered();
    plan(&workspace, ":list importers > available", no_files())
        .await
        .commit(&mut workspace, Duration::ZERO)
        .unwrap();
    workspace
        .register_importer("second".into(), fixture.clone())
        .unwrap();
    run_all(&mut workspace).await;
    let Data::List(names) = value(&workspace, "available") else {
        panic!("names")
    };
    assert_eq!(
        names,
        &[Data::Text("fixture".into()), Data::Text("second".into())]
    );
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn duplicate_imports_require_explicit_true_without_replacing_captured_handles() {
    let (mut workspace, fixture) = registered();
    plan(
        &workspace,
        ":import fixture file:first as:library",
        no_files(),
    )
    .await
    .commit(&mut workspace, Duration::ZERO)
    .unwrap();
    for flag in ["", " replace:false"] {
        let prepared = plan(
            &workspace,
            &format!(":import fixture file:second as:library{flag}"),
            no_files(),
        )
        .await;
        assert!(prepared.accepted().is_empty());
        assert!(
            prepared
                .diagnostics()
                .diagnostics
                .iter()
                .any(|d| d.code == "IMP004" && d.message.contains("replace:true"))
        );
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    }
    plan(&workspace, "library get > unchanged", no_files())
        .await
        .commit(&mut workspace, Duration::ZERO)
        .unwrap();
    run_all(&mut workspace).await;
    assert_eq!(value(&workspace, "unchanged"), &Data::Text("first".into()));
    plan(
        &workspace,
        ":import fixture file:second as:library replace:true\nlibrary get > changed",
        no_files(),
    )
    .await
    .commit(&mut workspace, Duration::ZERO)
    .unwrap();
    run_all(&mut workspace).await;
    assert_eq!(value(&workspace, "changed"), &Data::Text("second".into()));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn applied_replay_requires_exact_distinct_origin_evidence_and_preserves_frozen_values() {
    use wes_core::{Primitive, Provenance, Value};
    use wes_engine::imports::{ImportOrigin, ImportRecipe, ImportRequest, ImportSnapshot};
    let (workspace, _) = registered();
    let prepared = plan(
        &workspace,
        ":import fixture file:good as:library",
        no_files(),
    )
    .await;
    let mut command = prepared.record().unwrap().clone();
    command.text = ":import apply $expired".into();
    command.replay = command.text.clone();
    let request = ImportRequest::new(
        "fixture".into(),
        Some("library".into()),
        [(
            "file".into(),
            Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("good".into()),
                Provenance::default()
                    .with_fact("format", "synthetic/v1")
                    .cautioned(["synthetic caution".into()]),
            )
            .unwrap(),
        )]
        .into(),
    )
    .unwrap();
    command.imports = vec![ImportSnapshot::from_origin(
        request.clone(),
        ImportRecipe::new("fixture/v1".into(), "good".into()).unwrap(),
        ImportOrigin::Applied,
    )];
    let (base, fixture) = registered();
    let mut replay = ReplayWorkspace::new(base).unwrap();
    let rebuilt = replay
        .prepare(&command, CancellationToken::new())
        .await
        .unwrap();
    replay.apply(rebuilt).unwrap();
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    assert_eq!(*fixture.modes.lock().unwrap(), [ImportMode::Replay]);
    assert!(replay.workspace().catalogue().provider("library").is_some());
    for invalid in 0..5 {
        let mut command = command.clone();
        match invalid {
            0 => {
                command.imports.clear();
            }
            1 => {
                command.imports[0] =
                    ImportSnapshot::new(request.clone(), command.imports[0].recipe().clone());
            }
            2 => {
                command.replay = ":import fixture file:good as:library".into();
            }
            3 => {
                command.imports.push(command.imports[0].clone());
            }
            _ => {
                let mut args = request.arguments().clone();
                let value =
                    args["file"]
                        .clone()
                        .with_provenance(Provenance::default().with_policy(
                            &wes_core::flow::FlowPolicy::default().from_origin("restricted"),
                        ));
                args.insert("file".into(), value);
                command.imports[0] = ImportSnapshot::from_origin(
                    ImportRequest::new("fixture".into(), Some("library".into()), args).unwrap(),
                    command.imports[0].recipe().clone(),
                    ImportOrigin::Applied,
                );
            }
        }
        let (base, fixture) = registered();
        let mut replay = ReplayWorkspace::new(base).unwrap();
        assert!(
            replay
                .prepare(&command, CancellationToken::new())
                .await
                .is_err(),
            "case {invalid}"
        );
        assert!(replay.workspace().catalogue().provider("library").is_none());
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    }
}
