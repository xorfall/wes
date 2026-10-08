use std::{fs, path::Path};
use wes_adapters::{
    journal::{Durability, FileHistory, ReadLimits},
    workspaces::{FileWorkspaces, WorkspaceFileError},
};
use wes_engine::{
    graph::NodeId,
    history::{
        AppendReceipt, CallRecord, CommandRecord, HistoryCapture, HistoryCaptureLimits,
        HistoryCheckpoint, HistoryImage, JournalEntry, JournalSink, Persistence, Record,
        RecoveryEntry,
    },
    workspace::WorkspaceName,
};
fn temp() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn name(text: &str) -> WorkspaceName {
    WorkspaceName::new(text.into()).unwrap()
}
fn store(path: &Path) -> FileWorkspaces {
    FileWorkspaces::open(path, ReadLimits::default(), Durability::File).unwrap()
}
fn command(cell: &str) -> Record {
    Record::Journal(JournalEntry::Command(CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: cell.into(),
        text: ":help".into(),
        replay: ":help".into(),
        nodes: vec![NodeId::new("id0").unwrap()],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
    }))
}
fn image(cell: &str) -> HistoryImage {
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    capture.push(command(cell)).unwrap();
    capture
        .push(Record::Recovery(RecoveryEntry::Calling(CallRecord {
            node: NodeId::new("id0").unwrap(),
            run: wes_engine::runtime::RunId::new("run-a").unwrap(),
            cell: cell.into(),
            capability: "catalog echo".into(),
            safe: true,
            at: wes_core::Timestamp::new(100, 0).unwrap(),
        })))
        .unwrap();
    let receipt = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: 10,
    };
    capture.finish(HistoryCheckpoint {
        journal: receipt,
        recovery: receipt,
    })
}

#[test]
fn saved_generations_preserve_both_streams_and_loaded_writer_ownership() {
    let root = temp();
    let mut workspaces = store(root.path());
    let original = image("first");
    workspaces
        .save(&name("Çalışma CON:one"), &original)
        .unwrap();
    let (mut opened, captured) = workspaces.load(&name("Çalışma CON:one")).unwrap();
    assert_eq!(captured.journal(), original.journal());
    assert_eq!(captured.recovery(), original.recovery());
    assert_eq!(
        captured.checkpoint().journal.persistence,
        Persistence::FileSynced
    );
    assert!(workspaces.load(&name("Çalışma CON:one")).is_err()); // Exclusive generation writer.
    opened.append(&command("later")).unwrap();
    drop(opened);
    let (opened, captured) = workspaces.load(&name("Çalışma CON:one")).unwrap();
    assert_eq!(captured.journal().len(), 2); // Loading a name really opens its persistent workspace.
    drop(opened);
    drop(workspaces);
    let workspaces = store(root.path());
    assert_eq!(workspaces.names().unwrap(), vec![name("Çalışma CON:one")]);
}

#[test]
fn collection_preserves_referenced_and_active_generations_until_the_writer_finishes() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(DatasetPublicationProbe::new(root.path()));
    workspaces.save(&name("one"), &image("old")).unwrap();
    let (mut active, _) = workspaces.load(&name("one")).unwrap();
    workspaces.save(&name("one"), &image("new")).unwrap();
    let report = workspaces.collect_unused().unwrap();
    assert_eq!((report.removed, report.active, report.preserved), (0, 1, 0));
    active.append(&command("still-owned")).unwrap();
    assert_eq!(
        active
            .capture(HistoryCaptureLimits::default())
            .unwrap()
            .journal()
            .len(),
        2
    );
    drop(active);
    assert_eq!(workspaces.collect_unused().unwrap().removed, 1);
    let (_, current) = workspaces.load(&name("one")).unwrap();
    assert_eq!(current.journal(), image("new").journal());
    assert_eq!(workspaces.collect_unused().unwrap().removed, 0);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4); // marker, lock, pointer, generation
}

#[test]
fn malformed_pointer_prevents_any_collection_even_when_an_orphan_is_known() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("one"), &image("old")).unwrap();
    workspaces.save(&name("one"), &image("new")).unwrap();
    let count = fs::read_dir(root.path()).unwrap().count();
    fs::write(root.path().join("workspace-626164"), "broken").unwrap();
    assert!(workspaces.collect_unused().is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), count + 1);
    let (_, current) = workspaces.load(&name("one")).unwrap();
    assert_eq!(current.journal(), image("new").journal());
}

#[test]
fn collection_preserves_unrecognized_content_and_resumes_marked_retired_directories() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("one"), &image("old")).unwrap();
    let manifest = root.path().join("workspace-6f6e65");
    let old_id = fs::read_to_string(&manifest)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let old = root.path().join(format!("generation-{old_id}"));
    workspaces.save(&name("one"), &image("new")).unwrap();
    fs::write(old.join("personal"), b"preserve me").unwrap();
    let unmarked = root
        .path()
        .join("generation-00000000000000000000000000000000");
    fs::create_dir(&unmarked).unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().preserved, 2);
    assert_eq!(fs::read(old.join("personal")).unwrap(), b"preserve me");
    assert_eq!(fs::read_dir(&unmarked).unwrap().count(), 0);
    // A test-owned partial cleanup, with marker but no journal/lock, can be resumed.
    fs::remove_file(old.join("personal")).unwrap();
    let retired = root
        .path()
        .join(".wes-retired-11111111111111111111111111111111");
    fs::rename(old, &retired).unwrap();
    fs::remove_file(retired.join("journal.jsonl")).unwrap();
    fs::remove_file(retired.join(".wes-history.lock")).unwrap();
    let report = workspaces.collect_unused().unwrap();
    assert_eq!((report.removed, report.preserved), (1, 1));
    assert!(!retired.exists());
    assert!(unmarked.exists());
}

#[cfg(unix)]
#[test]
fn collection_does_not_follow_generation_or_payload_symlinks() {
    use std::os::unix::fs::symlink;
    let root = temp();
    let outside = temp();
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"outside").unwrap();
    let mut workspaces = store(root.path());
    workspaces.save(&name("one"), &image("old")).unwrap();
    let old_id = fs::read_to_string(root.path().join("workspace-6f6e65"))
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let old = root.path().join(format!("generation-{old_id}"));
    workspaces.save(&name("one"), &image("new")).unwrap();
    fs::remove_file(old.join("journal.jsonl")).unwrap();
    symlink(&sentinel, old.join("journal.jsonl")).unwrap();
    symlink(
        outside.path(),
        root.path()
            .join("generation-00000000000000000000000000000000"),
    )
    .unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().preserved, 2);
    assert_eq!(fs::read(sentinel).unwrap(), b"outside");
    assert!(
        old.join("journal.jsonl")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
}
#[test]
fn overwrite_is_atomic_and_does_not_mutate_a_previous_owned_generation() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("one"), &image("old")).unwrap();
    let (mut previous, _) = workspaces.load(&name("one")).unwrap();
    workspaces.save(&name("one"), &image("new")).unwrap();
    previous.append(&command("old-writer-only")).unwrap();
    let (_, replacement) = workspaces.load(&name("one")).unwrap();
    assert_eq!(replacement.journal(), image("new").journal());
    let old = previous.capture(HistoryCaptureLimits::default()).unwrap();
    assert_eq!(old.journal().len(), 2);
    assert_eq!(workspaces.names().unwrap(), vec![name("one")]);
}
#[test]
fn invalid_or_oversized_copy_preserves_previous_name() {
    let root = temp();
    let mut workspaces = store(root.path());
    let legacy = image("current");
    workspaces.save(&name("kept"), &legacy).unwrap();
    let (opened, captured) = workspaces.load(&name("kept")).unwrap();

    assert_eq!(captured.journal(), legacy.journal());
    assert_eq!(captured.recovery(), legacy.recovery());
    drop(opened);
    drop(workspaces);
    let mut workspaces = FileWorkspaces::open(
        root.path(),
        ReadLimits {
            lines: 0,
            ..Default::default()
        },
        Durability::File,
    )
    .unwrap();
    assert!(
        workspaces
            .save(&name("kept"), &image("replacement"))
            .is_err()
    );
    drop(workspaces);
    let mut workspaces = store(root.path());
    let (_, captured) = workspaces.load(&name("kept")).unwrap();
    assert_eq!(captured.recovery(), legacy.recovery());
    assert_eq!(workspaces.names().unwrap(), vec![name("kept")]);
}
#[test]
fn names_are_bounded_not_paths_and_missing_or_corrupt_targets_are_not_initialized() {
    for invalid in ["", " ", "../elsewhere", "a/b", "a\\b", "a..b", "\0", "\n"] {
        assert!(WorkspaceName::new(invalid.into()).is_err());
    }
    assert!(WorkspaceName::new("x".repeat(97)).is_err());
    let root = temp();
    let mut workspaces = store(root.path());
    assert!(matches!(
        workspaces.load(&name("missing")),
        Err(WorkspaceFileError::Missing)
    ));
    workspaces.save(&name("a"), &image("first")).unwrap();
    let manifest = root.path().join("workspace-61");
    let pointer = fs::read_to_string(&manifest).unwrap();
    let generation = pointer.lines().last().unwrap();
    let journal = root
        .path()
        .join(format!("generation-{generation}/journal.jsonl"));
    fs::write(&journal, b"{malformed\n").unwrap();
    assert!(workspaces.load(&name("a")).is_err());
    assert_eq!(fs::read(&journal).unwrap(), b"{malformed\n");
    fs::write(&manifest, b"wes.workspace\n99\n../../elsewhere\n").unwrap();
    assert!(workspaces.load(&name("a")).is_err());
    assert!(workspaces.save(&name("a"), &image("replacement")).is_err());
    assert!(workspaces.names().is_err());
}
#[cfg(unix)]
#[test]
fn managed_names_and_generation_directories_never_follow_symlinks() {
    use std::os::unix::fs::symlink;
    let root = temp();
    let outside = temp();
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"private unrelated content").unwrap();
    let mut workspaces = store(root.path());
    symlink(&sentinel, root.path().join("workspace-61")).unwrap();
    assert!(workspaces.load(&name("a")).is_err());
    assert!(workspaces.save(&name("a"), &image("replacement")).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"private unrelated content");
    let uuid = "00000000000000000000000000000000";
    fs::write(
        root.path().join("workspace-62"),
        format!("wes.workspace\n1\n{uuid}\n{uuid}\n"),
    )
    .unwrap();
    symlink(
        outside.path(),
        root.path().join(format!("generation-{uuid}")),
    )
    .unwrap();
    assert!(workspaces.load(&name("b")).is_err());
    assert!(!outside.path().join(".wes-history").exists());
}
#[test]
fn workspace_manifest_is_wes_v1_and_does_not_accept_the_previous_identity() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("a"), &image("current")).unwrap();
    let manifest = root.path().join("workspace-61");
    let current = fs::read_to_string(&manifest).unwrap();
    assert!(current.starts_with("wes.workspace\n1\n"));
    for header in [
        "unrelated.workspace\n2\n",
        "unrelated.workspace\n1\n",
        "wes.workspace\n2\n",
    ] {
        fs::write(&manifest, current.replacen("wes.workspace\n1\n", header, 1)).unwrap();
        assert!(workspaces.load(&name("a")).is_err());
    }
    fs::write(&manifest, current).unwrap();
    assert!(workspaces.load(&name("a")).is_ok());
}

#[test]
fn storage_roots_refuse_unowned_content_and_competing_owners() {
    let root = temp();
    let workspaces = store(root.path());
    assert!(FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).is_err());
    drop(workspaces);
    assert!(FileHistory::open(root.path(), ReadLimits::default(), Durability::File).is_err());
    let unrelated = temp();
    fs::write(unrelated.path().join("personal"), "keep").unwrap();
    assert!(
        FileWorkspaces::open(unrelated.path(), ReadLimits::default(), Durability::File).is_err()
    );
    assert_eq!(
        fs::read_to_string(unrelated.path().join("personal")).unwrap(),
        "keep"
    );
}

#[cfg(unix)]
#[test]
fn named_generation_acknowledges_directory_sync_when_selected() {
    let root = temp();
    let mut workspaces = FileWorkspaces::open(
        root.path(),
        ReadLimits::default(),
        Durability::FileAndDirectory,
    )
    .unwrap();
    workspaces.save(&name("durable"), &image("source")).unwrap();
    let (_, captured) = workspaces.load(&name("durable")).unwrap();
    assert_eq!(
        captured.checkpoint().journal.persistence,
        Persistence::FileAndDirectorySynced
    );
    assert_eq!(
        captured.checkpoint().recovery.persistence,
        Persistence::FileAndDirectorySynced
    );
}

/// A saved generation claims only what this host establishes; a refused stronger requirement
/// leaves the store as it was and usable with file durability.
#[test]
fn a_named_generation_claims_only_the_durability_this_host_establishes() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("kept"), &image("source")).unwrap();
    let (history, captured) = workspaces.load(&name("kept")).unwrap();
    assert_eq!(
        captured.checkpoint().journal.persistence,
        Persistence::FileSynced
    );
    assert_eq!(
        captured.checkpoint().recovery.persistence,
        Persistence::FileSynced
    );
    drop(history);
    drop(workspaces);
    let strong = FileWorkspaces::open(
        root.path(),
        ReadLimits::default(),
        Durability::FileAndDirectory,
    );
    assert_eq!(strong.is_ok(), Durability::FileAndDirectory.supported());
    drop(strong);
    let mut workspaces = store(root.path());
    let (_, reopened) = workspaces.load(&name("kept")).unwrap();
    assert_eq!(reopened.journal(), image("source").journal());
}

struct DatasetPublicationProbe {
    root: std::path::PathBuf,
    calls: std::sync::Mutex<Vec<(String, Vec<wes_engine::storage::ValueHandle>, Option<u64>)>>,
    fail_protect: std::sync::atomic::AtomicBool,
    fail_retire: std::sync::atomic::AtomicBool,
    retired: std::sync::Mutex<Vec<String>>,
}
impl DatasetPublicationProbe {
    fn new(root: &Path) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            root: root.into(),
            calls: Default::default(),
            fail_protect: false.into(),
            fail_retire: false.into(),
            retired: Default::default(),
        })
    }
}
impl wes_engine::history::RetainedDatasetPublication for DatasetPublicationProbe {
    fn protect(
        &self,
        generation: &str,
        handles: &[wes_engine::storage::ValueHandle],
    ) -> Result<Vec<wes_engine::storage::ValueHandle>, wes_engine::history::RecordError> {
        let id = uuid::Uuid::parse_str(generation)
            .unwrap()
            .simple()
            .to_string();
        let before = fs::metadata(self.root.join(format!("generation-{id}/journal.jsonl")))
            .ok()
            .map(|m| m.len());
        self.calls
            .lock()
            .unwrap()
            .push((generation.into(), handles.to_vec(), before));
        if self.fail_protect.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(wes_engine::history::RecordError::Limit(
                "synthetic root refusal",
            ));
        }
        Ok(handles.to_vec())
    }
    fn retire(&self, generation: &str) -> Result<(), wes_engine::history::RecordError> {
        let id = uuid::Uuid::parse_str(generation)
            .unwrap()
            .simple()
            .to_string();
        assert!(!self.root.join(format!("generation-{id}")).exists());
        assert!(!self.root.join(format!(".wes-retired-{id}")).exists());
        assert!(self.root.join(format!(".wes-retirement-{id}")).is_file());
        if self.fail_retire.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(wes_engine::history::RecordError::Limit(
                "synthetic root retirement refusal",
            ));
        }
        self.retired.lock().unwrap().push(generation.into());
        Ok(())
    }
}
fn retained_record() -> Record {
    Record::Journal(JournalEntry::Result(wes_engine::history::RetainedResult {
        node: NodeId::new("id0").unwrap(),
        run: wes_engine::runtime::RunId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        handle: wes_engine::storage::ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap(),
        retention: wes_engine::storage::Retention::Protected,
    }))
}

#[test]
fn retained_dataset_protection_precedes_seed_and_live_append_without_retaining_temporary_payloads()
{
    let root = temp();
    let probe = DatasetPublicationProbe::new(root.path());
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    workspaces.save(&name("one"), &image("initial")).unwrap();
    assert!(probe.calls.lock().unwrap().is_empty());
    let (mut history, _) = workspaces.load(&name("one")).unwrap();
    let before = history
        .capture(HistoryCaptureLimits::default())
        .unwrap()
        .checkpoint()
        .journal
        .end_offset;
    let kept = retained_record();
    history.append(&kept).unwrap();
    assert_eq!(probe.calls.lock().unwrap()[0].2, Some(before));
    let Record::Journal(JournalEntry::Result(retained)) = &kept else {
        panic!()
    };
    history
        .append(&Record::Journal(JournalEntry::Payload {
            node: retained.node.clone(),
            run: retained.run.clone(),
            handle: retained.handle.clone(),
        }))
        .unwrap();
    assert_eq!(probe.calls.lock().unwrap().len(), 1);
    let captured = history.capture(HistoryCaptureLimits::default()).unwrap();
    workspaces.save(&name("two"), &captured).unwrap();
    let calls = probe.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].1, vec![retained.handle.clone()]);
    assert_eq!(calls[1].2, None); // Dataset protection before any seed journal file.
}

#[test]
fn failed_dataset_protection_writes_no_retained_history_and_publishes_no_new_pointer() {
    let root = temp();
    let probe = DatasetPublicationProbe::new(root.path());
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    workspaces.save(&name("one"), &image("initial")).unwrap();
    let (mut history, _) = workspaces.load(&name("one")).unwrap();
    let before = history.capture(HistoryCaptureLimits::default()).unwrap();
    probe
        .fail_protect
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(history.append(&retained_record()).is_err());
    let after = history.capture(HistoryCaptureLimits::default()).unwrap();
    assert_eq!(after.journal(), before.journal());
    assert_eq!(
        after.checkpoint().journal.end_offset,
        before.checkpoint().journal.end_offset
    );
    assert_eq!(
        after.checkpoint().recovery.end_offset,
        before.checkpoint().recovery.end_offset
    );
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    capture.push(retained_record()).unwrap();
    let candidate = capture.finish(before.checkpoint());
    let identity = workspaces.identity(&name("one")).unwrap();
    assert!(workspaces.save(&name("one"), &candidate).is_err());
    assert_eq!(workspaces.identity(&name("one")).unwrap(), identity);
}

#[test]
fn physical_generation_retirement_preserves_identity_and_retries_root_release_after_restart() {
    let root = temp();
    let probe = DatasetPublicationProbe::new(root.path());
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    workspaces.save(&name("one"), &image("old")).unwrap();
    let old = workspaces
        .identity(&name("one"))
        .unwrap()
        .unwrap()
        .generation;
    let (history, _) = workspaces.load(&name("one")).unwrap();
    workspaces.save(&name("one"), &image("new")).unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().active, 1);
    assert!(probe.retired.lock().unwrap().is_empty());
    drop(history);
    probe
        .fail_retire
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(workspaces.collect_unused().is_err());
    assert!(!root.path().join(format!("generation-{old}")).exists());
    assert!(root.path().join(format!(".wes-retirement-{old}")).exists());
    drop(workspaces);
    probe
        .fail_retire
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    workspaces.collect_unused().unwrap();
    assert_eq!(
        *probe.retired.lock().unwrap(),
        vec![uuid::Uuid::parse_str(&old).unwrap().to_string()]
    );
    assert!(!root.path().join(format!(".wes-retirement-{old}")).exists());
    assert_eq!(workspaces.names().unwrap(), vec![name("one")]);
}

#[test]
fn owned_retirement_marker_recovers_the_empty_directory_window_but_preserves_unknown_content() {
    let root = temp();
    let probe = DatasetPublicationProbe::new(root.path());
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let retired = root.path().join(format!(".wes-retired-{id}"));
    fs::create_dir(&retired).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&retired, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        root.path().join(format!(".wes-retirement-{id}")),
        format!("wes.workspace.retirement\n1\n{id}\n"),
    )
    .unwrap();
    fs::write(retired.join("unknown"), b"keep").unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().preserved, 1);
    assert!(probe.retired.lock().unwrap().is_empty());
    fs::remove_file(retired.join("unknown")).unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().removed, 1);
    assert!(!retired.exists());
    assert_eq!(probe.retired.lock().unwrap().len(), 1);
}

#[test]
fn collection_uses_reserved_capacity_when_normal_publication_is_full() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.set_retained_dataset_publication(DatasetPublicationProbe::new(root.path()));
    for version in 0..4 {
        workspaces
            .save(&name("one"), &image(&format!("version-{version}")))
            .unwrap();
    }
    let limit = wes_budgets::get("workspace.catalogue") as usize;
    let occupied = fs::read_dir(root.path()).unwrap().count();
    for index in occupied..limit {
        fs::write(root.path().join(format!("capacity-{index}")), []).unwrap();
    }
    assert!(matches!(
        workspaces.save(&name("two"), &image("full")),
        Err(WorkspaceFileError::Capacity)
    ));
    let pending = root
        .path()
        .join(format!(".wes-pending-{}", uuid::Uuid::new_v4().simple()));
    fs::write(&pending, b"interrupted partial write").unwrap();
    let report = workspaces.collect_unused().unwrap();
    assert_eq!(report.removed, 3);
    assert!(!pending.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), limit - 3);
    workspaces
        .save(&name("two"), &image("after-cleanup"))
        .unwrap();
}

#[test]
fn collection_without_dataset_port_preserves_retirement_for_a_later_owned_retry() {
    let root = temp();
    let mut workspaces = store(root.path());
    workspaces.save(&name("one"), &image("first")).unwrap();
    let old = fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|s| s.starts_with("generation-"))
        .unwrap();
    workspaces.save(&name("one"), &image("second")).unwrap();
    assert_eq!(workspaces.collect_unused().unwrap().removed, 1);
    let marker = root.path().join(format!(
        ".wes-retirement-{}",
        old.strip_prefix("generation-").unwrap()
    ));
    assert!(marker.exists());
    let probe = DatasetPublicationProbe::new(root.path());
    workspaces.set_retained_dataset_publication(probe.clone());
    workspaces.collect_unused().unwrap();
    assert!(!marker.exists());
    assert_eq!(probe.retired.lock().unwrap().len(), 1);
}
