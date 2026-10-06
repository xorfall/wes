use std::{fs, num::NonZeroU64, path::Path};
use tempfile::TempDir;
use wes_adapters::{
    codec::{Limits, encode_value},
    storage::{Durability, FileValues, TieredValues},
};
use wes_core::{Data, Primitive, Provenance, Shape, Value};
use wes_engine::storage::{StoreError, ValueHandle, ValueStore};

fn private_temp() -> TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}

fn value(n: usize) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("x".repeat(n).into()),
        Provenance::default(),
    )
    .unwrap()
}
fn open(path: &Path, budget: Option<u64>) -> FileValues {
    FileValues::open(
        path,
        Limits::default(),
        Durability::File,
        budget.map(|n| NonZeroU64::new(n).unwrap()),
    )
    .unwrap()
}
fn tiered(live: &Path, archive: &Path, budget: Option<u64>) -> TieredValues {
    TieredValues::open(
        live,
        archive,
        Limits::default(),
        Durability::File,
        budget.map(|n| NonZeroU64::new(n).unwrap()),
    )
    .unwrap()
}

#[tokio::test]
async fn retention_reasons_promote_monotonically_reopen_and_count_unique_payloads() {
    use wes_engine::storage::{AutoKeep, Retention, StoreWorkerLimits, spawn_store};
    let root = private_temp();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let (worker, task) =
        spawn_store(tiered(&live, &archive, None), StoreWorkerLimits::default()).unwrap();
    let automatic = worker
        .publish(value(8), AutoKeep::default().into())
        .await
        .unwrap();
    assert_eq!(automatic.retention, Retention::Automatic);
    let temporary = worker
        .publish(value(9), AutoKeep::Never.into())
        .await
        .unwrap();
    assert_eq!(temporary.retention, Retention::Temporary);
    let kept = worker.retain(automatic.handle.clone()).await.unwrap();
    assert_eq!(kept.retention, Retention::Protected);
    assert_eq!(
        worker
            .retain_as(automatic.handle.clone(), Retention::Automatic)
            .await
            .unwrap()
            .retention,
        Retention::Protected
    );
    let usage = worker.retention_usage().await.unwrap();
    assert!(
        usage
            .classes
            .contains(&(Retention::Protected, 1, automatic.bytes))
    );
    assert!(
        usage
            .classes
            .contains(&(Retention::Temporary, 1, temporary.bytes))
    );
    assert_eq!(usage.live_bytes, automatic.bytes + temporary.bytes);
    assert_eq!(usage.archive_bytes, automatic.bytes);
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let mut store = tiered(&live, &archive, None);
    assert_eq!(
        store.retention(&automatic.handle).unwrap(),
        Retention::Protected
    );
    assert!(store.release(&automatic.handle).unwrap());
    assert!(store.read(&automatic.handle).unwrap().is_none());
    assert!(
        !archive
            .join(format!("{}.protected", automatic.handle))
            .exists()
    );
    assert!(
        !archive
            .join(format!("{}.automatic", automatic.handle))
            .exists()
    );
    assert_eq!(store.retention_usage().unwrap().archive_bytes, 0);
}

#[test]
fn missing_reason_is_unknown_and_corrupt_reason_never_authorizes_release() {
    use wes_engine::storage::Retention;
    let root = private_temp();
    let archive = root.path().join("archive");
    let mut store = tiered(&root.path().join("live"), &archive, None);
    let handle = store.store(&value(12)).unwrap();
    assert!(store.keep(&handle).unwrap());
    assert_eq!(store.retention(&handle).unwrap(), Retention::Unknown);
    assert_eq!(
        store
            .retention_usage()
            .unwrap()
            .classes
            .iter()
            .find(|(r, _, _)| *r == Retention::Unknown)
            .unwrap()
            .1,
        1
    );
    fs::write(archive.join(format!("{handle}.protected")), b"invalid").unwrap();
    assert!(store.retention(&handle).is_err());
    assert!(store.retention_usage().is_err());
    assert!(store.release(&handle).is_err());
    assert!(store.read(&handle).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn retention_markers_never_follow_symlinks_or_touch_external_targets() {
    use wes_engine::storage::Retention;
    let root = private_temp();
    let archive = root.path().join("archive");
    let mut store = tiered(&root.path().join("live"), &archive, None);
    let handle = store.store(&value(3)).unwrap();
    store.keep(&handle).unwrap();
    let outside = root.path().join("untouched");
    fs::write(&outside, b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, archive.join(format!("{handle}.protected"))).unwrap();
    assert!(
        store
            .keep_with_reason(&handle, Retention::Protected)
            .is_err()
    );
    assert!(store.release(&handle).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
    assert!(store.read(&handle).unwrap().is_some());
}

#[tokio::test]
async fn concurrent_publications_archive_each_value_before_the_next_live_eviction() {
    use wes_engine::{
        history::Persistence,
        storage::{AutoKeep, StoreWorkerLimits, spawn_store},
    };
    let root = private_temp();
    let (store, task) = spawn_store(
        tiered(
            &root.path().join("live"),
            &root.path().join("archive"),
            Some(1),
        ),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let mut publications = tokio::task::JoinSet::new();
    for n in 1..=16 {
        let store = store.clone();
        publications.spawn(async move {
            (
                n,
                store
                    .publish(value(n), AutoKeep::default().into())
                    .await
                    .unwrap(),
            )
        });
    }
    let mut outputs = vec![];
    while let Some(output) = publications.join_next().await {
        outputs.push(output.unwrap());
    }
    for (n, output) in outputs {
        assert!(output.kept);
        assert_eq!(output.retained_persistence, Persistence::FileSynced);
        assert_eq!(
            store
                .read(output.handle.clone())
                .await
                .unwrap()
                .unwrap()
                .value,
            value(n)
        );
        assert_eq!(
            store.encoded(output.handle).await.unwrap().unwrap().len() as u64,
            output.bytes
        );
    }
    let evicted = store
        .take_evicted(std::num::NonZeroUsize::new(32).unwrap())
        .await
        .unwrap();
    assert!(evicted.handles.is_empty());
    assert!(!evicted.more);
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn storage_worker_preserves_file_identity_and_releases_the_directory_lock_at_shutdown() {
    use wes_engine::storage::{StoreWorkerLimits, spawn_store};
    let root = private_temp();
    let (store, task) = spawn_store(open(root.path(), None), StoreWorkerLimits::default()).unwrap();
    let original = value(32).with_provenance(
        Provenance::default()
            .with_fact("source", "synthetic")
            .cautioned(["test:caution".into()]),
    );
    let handle = store.store(original.clone()).await.unwrap();
    assert_eq!(
        store.read(handle.clone()).await.unwrap().unwrap().value,
        original
    );
    let bytes = store.encoded(handle.clone()).await.unwrap().unwrap();
    assert_eq!(bytes, encode_value(&original, Limits::default()).unwrap());
    assert_eq!(
        store.size(handle.clone()).await.unwrap(),
        Some(bytes.len() as u64)
    );
    assert!(matches!(
        FileValues::open(root.path(), Limits::default(), Durability::File, None),
        Err(StoreError::Locked)
    ));
    let report = store.shutdown().await.unwrap();
    assert_eq!(report.failed, 0);
    let reopened = open(root.path(), None);
    assert_eq!(reopened.read(&handle).unwrap().unwrap().value, original);
    task.join().await.unwrap();
}

#[test]
fn stored_values_survive_reopening_and_keep_their_handle() {
    let root = private_temp();
    let handle;
    {
        let mut store = open(root.path(), None);
        handle = store.store(&value(10)).unwrap();
        assert_eq!(store.read(&handle).unwrap().unwrap().value, value(10));
        assert_eq!(store.handles().unwrap(), std::slice::from_ref(&handle));
    }
    let store = open(root.path(), None);
    let read = store.read(&handle).unwrap().unwrap();
    assert_eq!(read.value, value(10));

    assert_eq!(
        store.size(&handle).unwrap().unwrap() as usize,
        store.encoded(&handle).unwrap().unwrap().len()
    );
}

#[test]
fn a_second_writer_is_rejected_until_the_first_store_closes() {
    let root = private_temp();
    let store = open(root.path(), None);
    assert!(matches!(
        FileValues::open(root.path(), Limits::default(), Durability::File, None),
        Err(StoreError::Locked)
    ));
    drop(store);
    assert!(FileValues::open(root.path(), Limits::default(), Durability::File, None).is_ok());
}

#[test]
fn unowned_directories_are_not_modified_or_claimed() {
    let root = private_temp();
    fs::write(root.path().join("personal.txt"), "keep me").unwrap();
    assert!(matches!(
        FileValues::open(root.path(), Limits::default(), Durability::File, None),
        Err(StoreError::UnownedDirectory)
    ));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(root.path().join("personal.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn live_budget_evicts_oldest_but_always_keeps_the_newest() {
    let root = private_temp();
    let size = encode_value(&value(100), Limits::default()).unwrap().len() as u64;
    let mut store = open(root.path(), Some(size * 2));
    let first = store.store(&value(100)).unwrap();
    let second = store.store(&value(100)).unwrap();
    let third = store.store(&value(100)).unwrap();
    assert!(store.read(&first).unwrap().is_none());
    assert!(store.read(&second).unwrap().is_some());
    assert!(store.read(&third).unwrap().is_some());
    assert_eq!(store.take_evicted(usize::MAX).unwrap().handles, [first]);
    assert!(store.take_evicted(usize::MAX).unwrap().handles.is_empty());
    drop(store);
    let tiny = private_temp();
    let mut store = open(tiny.path(), Some(1));
    let handle = store.store(&value(500)).unwrap();
    assert!(store.size(&handle).unwrap().unwrap() > 1);
    assert!(store.read(&handle).unwrap().is_some());
}

#[test]
fn live_budget_evicts_in_publication_order_when_file_times_are_equal() {
    // Coarse filesystem clocks give successive results one timestamp; random handles must not
    // decide which of them is oldest.
    let equal = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    for _ in 0..16 {
        let root = private_temp();
        let size = encode_value(&value(100), Limits::default()).unwrap().len() as u64;
        let mut store = open(root.path(), Some(size * 2));
        let first = store.store(&value(100)).unwrap();
        let second = store.store(&value(100)).unwrap();
        for handle in [&first, &second] {
            fs::File::options()
                .write(true)
                .open(root.path().join(format!("{handle}.json")))
                .unwrap()
                .set_modified(equal)
                .unwrap();
        }

        store.store(&value(100)).unwrap();

        assert!(store.read(&first).unwrap().is_none());
        assert!(store.read(&second).unwrap().is_some());
    }
}

#[test]
fn eviction_batches_preserve_unexamined_notifications_and_report_more_candidates() {
    let root = private_temp();
    let mut store = open(root.path(), Some(1));
    let first = store.store(&value(10)).unwrap();
    let second = store.store(&value(20)).unwrap();
    store.store(&value(30)).unwrap();
    let empty = store.take_evicted(0).unwrap();
    assert!(empty.handles.is_empty());
    assert!(empty.more);
    let batch = store.take_evicted(1).unwrap();
    assert_eq!(batch.handles, [first]);
    assert!(batch.more);
    let batch = store.take_evicted(1).unwrap();
    assert_eq!(batch.handles, [second]);
    assert!(!batch.more);
}

#[test]
fn an_archived_eviction_can_make_an_empty_batch_without_hiding_later_candidates() {
    let root = private_temp();
    let mut store = tiered(
        &root.path().join("live"),
        &root.path().join("archive"),
        Some(1),
    );
    let kept = store.store(&value(10)).unwrap();
    store.keep(&kept).unwrap();
    let gone = store.store(&value(20)).unwrap();
    store.store(&value(30)).unwrap();
    let batch = store.take_evicted(1).unwrap();
    assert!(batch.handles.is_empty());
    assert!(batch.more);
    assert!(store.read(&kept).unwrap().is_some());
    let batch = store.take_evicted(1).unwrap();
    assert_eq!(batch.handles, [gone]);
    assert!(!batch.more);
}

#[test]
fn budget_and_drop_never_delete_unrelated_entries_or_subdirectories() {
    let root = private_temp();
    let mut store = open(root.path(), Some(1));
    fs::write(root.path().join("personal.txt"), "keep").unwrap();
    fs::create_dir(root.path().join("other")).unwrap();
    store.store(&value(50)).unwrap();
    store.store(&value(60)).unwrap();
    drop(store);
    assert_eq!(
        fs::read_to_string(root.path().join("personal.txt")).unwrap(),
        "keep"
    );
    assert!(root.path().join("other").is_dir());
}

#[test]
fn adopted_identity_is_idempotent_but_never_overwritten_by_different_bytes() {
    let root = private_temp();
    let mut store = open(root.path(), None);
    let handle = ValueHandle::fresh();
    let first = encode_value(&value(1), Limits::default()).unwrap();
    let second = encode_value(&value(2), Limits::default()).unwrap();
    store.adopt(&handle, &first).unwrap();
    store.adopt(&handle, &first).unwrap();
    assert!(matches!(
        store.adopt(&handle, &second),
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.encoded(&handle).unwrap().unwrap(), first);
}

#[test]
fn malformed_or_oversized_adoption_cannot_publish_a_handle() {
    let root = private_temp();
    let mut store = open(root.path(), None);
    let handle = ValueHandle::fresh();
    assert!(store.adopt(&handle, b"{broken").is_err());
    assert!(store.read(&handle).unwrap().is_none());
    assert!(store.handles().unwrap().is_empty());
    drop(store);
    let mut store = FileValues::open(
        root.path(),
        Limits {
            bytes: 1,
            ..Limits::default()
        },
        Durability::File,
        None,
    )
    .unwrap();
    assert!(store.store(&value(1)).is_err());
    assert!(store.handles().unwrap().is_empty());
}

#[test]
fn incomplete_pending_files_are_not_returned_as_values_after_restart() {
    let root = private_temp();
    {
        let _store = open(root.path(), None);
    }
    let pending = root
        .path()
        .join(".wes-pending-11111111-2222-3333-4444-555555555555");
    fs::write(&pending, b"{partial").unwrap();
    let store = open(root.path(), None);
    assert!(store.handles().unwrap().is_empty());
    assert!(
        store
            .read(&ValueHandle::new("11111111-2222-3333-4444-555555555555").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn kept_values_survive_live_eviction_and_a_new_live_session() {
    let root = private_temp();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let kept;
    {
        let mut store = tiered(&live, &archive, Some(1));
        kept = store.store(&value(10)).unwrap();
        assert!(store.keep(&kept).unwrap());
        assert!(store.keep(&kept).unwrap());
        store.store(&value(20)).unwrap();
        assert!(store.take_evicted(usize::MAX).unwrap().handles.is_empty());
        assert!(store.is_kept(&kept).unwrap());
        assert_eq!(store.read(&kept).unwrap().unwrap().value, value(10));
    }
    let mut store = tiered(&root.path().join("new-live"), &archive, Some(1));
    assert_eq!(store.read(&kept).unwrap().unwrap().value, value(10));
    assert_eq!(
        store.archived_handles().unwrap(),
        std::slice::from_ref(&kept)
    );
    assert!(store.keep(&kept).unwrap());
}

#[test]
fn explicit_release_removes_both_live_and_kept_copies() {
    let root = private_temp();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let mut store = tiered(&live, &archive, None);
    let handle = store.store(&value(10)).unwrap();
    store.keep(&handle).unwrap();
    assert!(store.release(&handle).unwrap());
    assert!(store.read(&handle).unwrap().is_none());
    assert!(!store.is_kept(&handle).unwrap());
    assert!(!store.release(&handle).unwrap());
    assert!(!store.keep(&handle).unwrap());
}

#[test]
fn historical_value_adoption_is_rejected_without_writing() {
    let root = private_temp();
    let mut store = open(root.path(), None);
    let handle = ValueHandle::fresh();
    let bytes = br#"{"type":{"kind":"primitive","name":"BYTES"},"provenance":{"source":"legacy"},"data":"AQI="}"#;
    assert!(store.adopt(&handle, bytes).is_err());
    assert!(!root.path().join(format!("{handle}.json")).exists());
}

#[test]
fn corrupted_existing_files_are_errors_not_absence_or_empty_success() {
    let root = private_temp();
    let mut store = open(root.path(), None);
    let handle = store.store(&value(10)).unwrap();
    fs::write(root.path().join(format!("{handle}.json")), b"").unwrap();
    assert!(store.read(&handle).is_err());
}

#[test]
fn path_fragments_windows_devices_and_noncanonical_handles_are_refused() {
    for handle in [
        "",
        "../outside",
        "/tmp/outside",
        "a\\b",
        "CON",
        "nul",
        "11111111222233334444555555555555",
        "{11111111-2222-3333-4444-555555555555}",
        "AAAAAAAA-2222-3333-4444-555555555555",
    ] {
        assert!(ValueHandle::new(handle).is_err(), "{handle}");
    }
    assert!(ValueHandle::new("11111111-2222-3333-4444-555555555555").is_ok());
}

#[cfg(unix)]
#[test]
fn symlinked_values_cannot_read_or_remove_files_outside_the_store() {
    let root = private_temp();
    let outside = private_temp();
    let target = outside.path().join("do-not-touch");
    fs::write(&target, "external bytes").unwrap();
    let mut store = open(root.path(), None);
    let handle = ValueHandle::fresh();
    std::os::unix::fs::symlink(&target, root.path().join(format!("{handle}.json"))).unwrap();
    assert!(store.read(&handle).is_err());
    assert!(store.release(&handle).is_err());
    assert!(store.handles().is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "external bytes");
}

#[cfg(unix)]
#[test]
fn private_permissions_and_directory_sync_are_applied_and_insecure_roots_are_not_chmoded() {
    use std::os::unix::fs::PermissionsExt;
    let root = private_temp();
    let path = root.path().join("values");
    let mut store =
        FileValues::open(&path, Limits::default(), Durability::FileAndDirectory, None).unwrap();
    let handle = store.store(&value(1)).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(path.join(format!("{handle}.json")))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(store.release(&handle).unwrap());
    let insecure = root.path().join("insecure");
    fs::create_dir(&insecure).unwrap();
    fs::set_permissions(&insecure, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        FileValues::open(&insecure, Limits::default(), Durability::File, None),
        Err(StoreError::InsecureDirectory)
    ));
    assert_eq!(
        fs::metadata(&insecure).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn live_and_archive_cannot_accidentally_share_the_same_directory() {
    let root = private_temp();
    assert!(matches!(
        TieredValues::open(
            root.path(),
            root.path(),
            Limits::default(),
            Durability::File,
            None
        ),
        Err(StoreError::Locked)
    ));
}

#[test]
fn incorrect_ownership_markers_are_rejected_before_creating_a_lock_file() {
    let root = private_temp();
    fs::write(root.path().join(".wes-value-store"), "belongs elsewhere").unwrap();
    assert!(matches!(
        FileValues::open(root.path(), Limits::default(), Durability::File, None),
        Err(StoreError::UnownedDirectory)
    ));
    assert!(!root.path().join(".wes-values.lock").exists());
}

#[cfg(unix)]
#[test]
fn failed_archive_inspection_does_not_lose_pending_eviction_notifications() {
    let root = private_temp();
    let archive = root.path().join("archive");
    let mut store = tiered(&root.path().join("live"), &archive, Some(1));
    let old = store.store(&value(10)).unwrap();
    store.store(&value(20)).unwrap();
    let target = root.path().join("unrelated");
    fs::write(&target, "safe").unwrap();
    let link = archive.join(format!("{old}.json"));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(store.take_evicted(usize::MAX).is_err());
    fs::remove_file(link).unwrap();
    assert_eq!(store.take_evicted(usize::MAX).unwrap().handles, [old]);
    assert!(store.take_evicted(usize::MAX).unwrap().handles.is_empty());
}

#[tokio::test]
async fn protected_publication_keeps_exact_value_in_one_job_and_recovers_after_reopen() {
    use wes_engine::storage::{PublicationPolicy, Retention, StoreWorkerLimits, spawn_store};
    let root = private_temp();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let original = value(73);
    let (worker, task) = spawn_store(
        tiered(&live, &archive, Some(1)),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let published = worker
        .publish(original.clone(), PublicationPolicy::Protected)
        .await
        .unwrap();
    assert!(published.kept);
    assert_eq!(published.retention, Retention::Protected);
    assert_eq!(
        worker
            .read(published.handle.clone())
            .await
            .unwrap()
            .unwrap()
            .value,
        original
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let (worker, task) = spawn_store(
        tiered(&live, &archive, Some(1)),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let recovered = worker
        .recover(published.handle.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.loaded.value, original);
    assert_eq!(recovered.retention, Retention::Protected);
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

/// A value store opens with a durability this host establishes and refuses one it does not,
/// without disturbing the values already kept.
#[test]
fn a_value_store_opens_only_with_a_durability_this_host_establishes() {
    let root = private_temp();
    let path = root.path().join("values");
    let mut store = open(&path, None);
    let handle = store.store(&value(8)).unwrap();
    drop(store);
    let strong = FileValues::open(&path, Limits::default(), Durability::FileAndDirectory, None);
    assert_eq!(strong.is_ok(), Durability::FileAndDirectory.supported());
    drop(strong);
    let store = open(&path, None);
    assert!(store.read(&handle).unwrap().is_some());
}
