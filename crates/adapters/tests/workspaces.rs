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
