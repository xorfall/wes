use serde_json::{Value, json};
use std::{num::NonZeroUsize, sync::Arc};
use wes_adapters::{
    codec::{
        Limits,
        history::{decode_journal, encode_journal},
    },
    journal::{Durability, FileHistory, ReadLimits},
    workspaces::FileWorkspaces,
};
use wes_core::environments::{CapturedSource, CapturedSources, Package};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    environments::{EnvironmentRecord, Registry},
    history::{HistoryCaptureLimits, JournalEntry},
    recording::{RecorderLimits, spawn_recorder},
    session::{self, RecordingMode},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::{Workspace, WorkspaceName},
};

fn inputs() -> (String, CapturedSources) {
    let yaml = "version: 1\ntargets: {local: {kind: local}}\nenvironments:\n  dev: {imports: {api: {source: {kind: spec, file: never-open.json}, bind: {target: local}}}}\n".to_string();
    let mut sources = CapturedSources::default();
    for key in Package::parse(&yaml).unwrap().required_sources() {
        sources
            .insert(
                key,
                CapturedSource::new("synthetic/v1", "Türkçe 🦀\n\"exact\"\t\\bytes").unwrap(),
            )
            .unwrap();
    }
    (yaml, sources)
}
fn record() -> EnvironmentRecord {
    let (yaml, sources) = inputs();
    let mut registry = Registry::default();
    let plan = registry
        .plan(&Package::parse(&yaml).unwrap(), &sources)
        .unwrap();
    registry.apply(plan).unwrap();
    EnvironmentRecord::new(
        "00000000-0000-4000-8000-000000000001".into(),
        yaml,
        sources,
        Default::default(),
        registry.revisions(),
    )
    .unwrap()
}
fn wire() -> Value {
    serde_json::from_slice(
        &encode_journal(&JournalEntry::Environments(record()), Limits::default()).unwrap(),
    )
    .unwrap()
}
fn decode(
    value: &Value,
) -> Result<
    wes_adapters::codec::history::DecodedRecord<JournalEntry>,
    wes_adapters::codec::CodecError,
> {
    decode_journal(&serde_json::to_vec(value).unwrap(), Limits::default())
}

#[test]
fn current_roundtrip_preserves_exact_evidence_and_charge_bounds_encoding() {
    let record = record();
    let entry = JournalEntry::Environments(record.clone());
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(wire()["version"], 1);
    assert!(record.charge() >= bytes.len() as u64);
    let decoded = decode_journal(&bytes, Limits::default()).unwrap();
    assert_eq!(decoded.entry, entry);

    let restored = match decoded.entry {
        JournalEntry::Environments(r) => r,
        _ => unreachable!(),
    };
    let mut registry = Registry::default();
    registry
        .apply(restored.prepare(&registry).unwrap())
        .unwrap();
    assert_eq!(registry.revisions(), *record.after());
    assert!(!format!("{record:?}").contains("exact"));
}

#[test]
fn environment_records_cannot_be_downgraded_or_claim_unknown_versions() {
    let value = wire();
    assert!(decode(&value["entry"]).is_err());
    for version in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 99] {
        let mut bad = value.clone();
        bad["version"] = json!(version);
        assert!(decode(&bad).is_err(), "version {version}");
    }
}

#[test]
fn unknown_fields_duplicate_sources_missing_evidence_and_bad_identities_reject() {
    let value = wire();
    let mut mutations = vec![];
    let mut bad = value.clone();
    bad["entry"]["grant"] = json!(true);
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["sources"][0]["secretValue"] = json!("forbidden");
    mutations.push(bad);
    let mut bad = value.clone();
    let duplicate = bad["entry"]["sources"][0].clone();
    bad["entry"]["sources"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["sources"] = json!([]);
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["after"] = json!({});
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["after"]["dev"] = json!("not-a-revision");
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["id"] = json!("not-a-uuid");
    mutations.push(bad);
    let mut bad = value.clone();
    bad["entry"]["sources"][0]["kind"] = json!("unknown");
    mutations.push(bad);
    let mut bad = value;
    bad["entry"]["yaml"] = json!("version: 999");
    mutations.push(bad);
    for bad in mutations {
        assert!(decode(&bad).is_err());
    }
    let bytes = serde_json::to_string(&wire()).unwrap().replacen(
        "\"before\":",
        "\"before\":{},\"before\":",
        1,
    );
    assert!(decode_journal(bytes.as_bytes(), Limits::default()).is_err());
}

struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("held environment replay reopened a path")
    }
}
fn temp() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}

#[tokio::test]
async fn real_file_append_checkpoint_save_load_and_held_session_restore_preserve_definitions() {
    let root = temp();
    let collection = temp();
    let history = FileHistory::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
    let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let (handle, task) = session::spawn(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let (yaml, sources) = inputs();
    let plan = handle.plan_environments(yaml, sources).await.unwrap();
    let expected = plan.revisions().clone();
    assert!(handle.apply_environments(plan).await.unwrap().recorded);
    let checkpoint = handle.checkpoint().await.unwrap();
    let mut workspaces =
        FileWorkspaces::open(collection.path(), ReadLimits::default(), Durability::File).unwrap();
    let name = WorkspaceName::new("Environment QA".into()).unwrap();
    workspaces.save(&name, checkpoint.history()).unwrap();
    drop(checkpoint);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();

    let (mut loaded_writer, loaded) = workspaces.load(&name).unwrap();
    assert_eq!(loaded.journal().len(), 1);

    assert!(matches!(loaded.journal()[0], JournalEntry::Environments(_)));
    let before = loaded_writer
        .capture(HistoryCaptureLimits::default())
        .unwrap()
        .checkpoint();
    let (recorder, writer) = spawn_recorder(loaded_writer, RecorderLimits::default()).unwrap();
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        None,
        loaded,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().environments, 1);
    assert_eq!(restored.workspace().environments().revisions(), expected);
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(handle.environment_revisions().await.unwrap(), expected);
    let checkpoint = handle.checkpoint().await.unwrap();
    assert_eq!(
        checkpoint.history().checkpoint().journal.end_offset,
        before.journal.end_offset
    );
    assert_eq!(
        checkpoint.history().checkpoint().recovery.end_offset,
        before.recovery.end_offset
    );
    assert!(checkpoint.history().recovery().is_empty());
    assert!(
        checkpoint
            .history()
            .journal()
            .iter()
            .all(|entry| matches!(entry, JournalEntry::Environments(_)))
    );
    drop(checkpoint);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
