use std::{fs, sync::Arc, time::Duration};
use wes_adapters::type_sources::FileTypeSources;
use wes_engine::{
    driver::{CancellationToken, Executor},
    runtime::{Effect, Outcome},
    tasks::TaskExecutor,
    type_sources::{TypeInput, TypeSourceCapture, TypeSourceError, TypeSourceReader},
    workspace::{Preparation, PreparedMeta, Workspace, WorkspaceError},
};
use wes_language::{SourceText, Statement, parse};

fn statement(text: &str) -> Statement {
    let parsed = parse(&SourceText::new("test", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 1);
    parsed.script.statements.into_iter().next().unwrap()
}
fn meta(workspace: &Workspace, text: &str) -> PreparedMeta {
    let Preparation::Meta(prepared) = workspace.prepare(&statement(text)).unwrap() else {
        panic!("meta continuation expected")
    };
    prepared
}

#[test]
fn reads_exact_utf8_from_captured_base_absolute_paths_and_parent_paths() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let source = "\u{feff}# 目录\r\ntypes: {Category: {base: Text, enum: [books]}}\r\n";
    let file = directory.path().join("目录.yaml");
    fs::write(&file, source).unwrap();
    let reader = FileTypeSources::new(&nested).unwrap();
    assert_eq!(reader.read("../目录.yaml", source.len()).unwrap(), source);
    assert_eq!(
        reader.read(file.to_str().unwrap(), source.len()).unwrap(),
        source
    );
    assert!(FileTypeSources::new(&file).is_err());
    assert!(!format!("{reader:?}").contains(directory.path().to_str().unwrap()));
    fs::remove_file(&file).unwrap();
    assert!(!file.exists());
}

#[test]
fn bounds_apply_to_bytes_and_reject_invalid_utf8_without_creating_or_modifying_files() {
    let directory = tempfile::tempdir().unwrap();
    let reader = FileTypeSources::new(directory.path()).unwrap();
    fs::write(directory.path().join("utf8"), "é").unwrap();
    assert_eq!(reader.read("utf8", 1), Err(TypeSourceError::TooLarge));
    assert_eq!(reader.read("utf8", 2).unwrap(), "é");
    fs::write(directory.path().join("binary"), [0xff, 0]).unwrap();
    assert_eq!(reader.read("binary", 2), Err(TypeSourceError::InvalidText));
    fs::write(directory.path().join("empty"), []).unwrap();
    assert_eq!(reader.read("empty", 0).unwrap(), "");
    assert_eq!(reader.read("utf8", 0), Err(TypeSourceError::TooLarge));
    assert_eq!(
        reader.read("missing-private-path", 10),
        Err(TypeSourceError::Unavailable)
    );
    assert_eq!(reader.read(".", 10), Err(TypeSourceError::Unavailable));
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 3);
    assert_eq!(
        fs::read(directory.path().join("binary")).unwrap(),
        [0xff, 0]
    );
}

#[cfg(unix)]
#[test]
fn ordinary_file_symlinks_remain_readable_and_nonregular_inputs_fail_without_waiting_for_a_writer()
{
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("target.yaml"), "types: {}").unwrap();
    symlink(
        target.path().join("target.yaml"),
        directory.path().join("linked.yaml"),
    )
    .unwrap();
    symlink(target.path(), directory.path().join("dir-link")).unwrap();
    let pipe = directory.path().join("input.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    let reader = FileTypeSources::new(directory.path()).unwrap();
    assert_eq!(reader.read("linked.yaml", 100).unwrap(), "types: {}");
    assert_eq!(
        reader.read("dir-link", 100),
        Err(TypeSourceError::Unavailable)
    );
    assert_eq!(
        reader.read("input.fifo", 100),
        Err(TypeSourceError::Unavailable)
    );
    assert_eq!(
        fs::read_to_string(target.path().join("target.yaml")).unwrap(),
        "types: {}"
    );
}

#[tokio::test]
async fn file_load_binds_the_requested_snapshot_and_reports_success_only_at_commit() {
    let directory = tempfile::tempdir().unwrap();
    let original = "types: {Positive: {base: Int, min: 1}}\n";
    fs::write(directory.path().join("types.yaml"), original).unwrap();
    let reader = Arc::new(FileTypeSources::new(directory.path()).unwrap());
    let mut capture = TypeSourceCapture::live(reader);
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let prepared = meta(&workspace, ":package load path:types.yaml");
    let TypeInput::File(path) = prepared.type_source().unwrap() else {
        panic!("file input expected");
    };
    let package = capture.read(&path, CancellationToken::new()).await.unwrap();
    fs::write(
        directory.path().join("types.yaml"),
        "types: {Changed: {base: Text}}",
    )
    .unwrap();
    let loaded = workspace.prepare_type_load(prepared, &package).unwrap();
    assert!(loaded.diagnostics().is_empty());
    assert!(workspace.contracts().resolve("Positive").is_err());
    let applied = workspace.commit(loaded).unwrap();
    assert!(applied.node.is_none());
    assert_eq!(applied.diagnostics[0].code, "TYP000");
    assert_eq!(applied.diagnostics[0].message, "loaded 1 type definitions");
    assert!(workspace.contracts().resolve("Changed").is_err());
    capture.accept(&package).unwrap();
    let recorded = capture.finish();
    assert_eq!(recorded["types.yaml"], original);
    fs::remove_file(directory.path().join("types.yaml")).unwrap();
    let mut replay = TypeSourceCapture::replay(recorded).unwrap();
    let mut restored = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let meta = meta(&restored, ":package load path:types.yaml");
    let TypeInput::File(path) = meta.type_source().unwrap() else {
        panic!("file input expected");
    };
    let package = replay.read(&path, CancellationToken::new()).await.unwrap();
    let prepared = restored.prepare_type_load(meta, &package).unwrap();
    restored.commit(prepared).unwrap();
    assert!(restored.contracts().resolve("Positive").is_ok());
    assert!(restored.runtime().graph().is_empty());
}

#[tokio::test]
async fn source_path_mismatch_wrong_operation_and_stale_continuation_never_load_a_package() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("a.yaml"),
        "types: {Positive: {base: Int, min: 1}}",
    )
    .unwrap();
    let mut capture =
        TypeSourceCapture::live(Arc::new(FileTypeSources::new(directory.path()).unwrap()));
    let package = capture
        .read("a.yaml", CancellationToken::new())
        .await
        .unwrap();
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    assert!(
        workspace
            .prepare_type_load(meta(&workspace, ":package load path:b.yaml"), &package)
            .is_err()
    );
    let wrong = meta(&workspace, ":workspace save \"session\"");
    assert!(matches!(
        wrong.type_source(),
        Err(TypeSourceError::InvalidInput)
    ));
    assert!(workspace.prepare_type_load(wrong, &package).is_err());
    let stale = meta(&workspace, ":package load path:a.yaml");
    let Preparation::Change(definition) = workspace
        .prepare(&statement(":def named as example get value:?value"))
        .unwrap()
    else {
        panic!()
    };
    workspace.commit(definition).unwrap();
    assert!(matches!(
        workspace.prepare_type_load(stale, &package),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(workspace.contracts().resolve("Positive").is_err());
    assert!(capture.finish().is_empty());
}

#[tokio::test]
async fn a_file_package_and_its_checked_value_share_one_uncommitted_declaration_draft() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("types.yaml"),
        "types: {Positive: {base: Int, min: 1}}",
    )
    .unwrap();
    let mut capture =
        TypeSourceCapture::live(Arc::new(FileTypeSources::new(directory.path()).unwrap()));
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut draft = workspace.draft().unwrap();
    let Preparation::Meta(meta) = draft
        .prepare(&statement(":package load path:types.yaml"))
        .unwrap()
    else {
        panic!()
    };
    let TypeInput::File(path) = meta.type_source().unwrap() else {
        panic!("file input expected");
    };
    let package = capture.read(&path, CancellationToken::new()).await.unwrap();
    let prepared = draft.prepare_type_load(meta, &package).unwrap();
    draft.stage(prepared).unwrap();
    capture.accept(&package).unwrap();
    let Preparation::Change(check) = draft
        .prepare(&statement(":type check \"7\" as:Positive > checked"))
        .unwrap()
    else {
        panic!()
    };
    draft.stage(check).unwrap();
    assert!(workspace.contracts().resolve("Positive").is_err());
    assert!(workspace.runtime().graph().is_empty());
    let applied = workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(applied.changes[0].diagnostics[0].code, "TYP000");
    let work = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let run = work.run.clone();
    assert!(workspace.enter(&run));
    let report = TaskExecutor::ephemeral()
        .execute(work, CancellationToken::new())
        .await;
    assert!(
        matches!(&report.outcome, Outcome::Produced(value) if matches!(value.data(), wes_core::Data::Int(7)))
    );
    workspace.complete(&run, report.outcome, Duration::from_secs(1));
    assert_eq!(capture.finish().len(), 1);
}

#[tokio::test]
async fn invalid_package_is_not_promoted_or_partly_loaded() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("types.yaml"),
        "types: {Good: {base: Text}, Bad: {base: Missing}}",
    )
    .unwrap();
    let mut capture =
        TypeSourceCapture::live(Arc::new(FileTypeSources::new(directory.path()).unwrap()));
    let workspace = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let prepared = meta(&workspace, ":package load path:types.yaml");
    let TypeInput::File(path) = prepared.type_source().unwrap() else {
        panic!("file input expected");
    };
    let package = capture.read(&path, CancellationToken::new()).await.unwrap();
    assert!(workspace.prepare_type_load(prepared, &package).is_err());
    assert!(workspace.contracts().resolve("Good").is_err());
    assert!(capture.finish().is_empty());
}
