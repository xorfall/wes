use std::{num::NonZeroU64, sync::Arc};
use wes_adapters::{
    codec::Limits,
    storage::{Durability, TieredValues},
};
use wes_core::{Data, Primitive, Shape, Value};
use wes_engine::storage::{CHUNK_BYTES, LeafSelection, Retention, StoreError, ValueStore};
fn shell() -> Value {
    Value::new(
        Shape::Primitive(Primitive::Bytes),
        Data::Bytes(vec![].into()),
        Default::default(),
    )
    .unwrap()
}

#[test]
fn encrypted_segmented_sources_keep_exact_identity_and_refuse_temporary_promotion() {
    use wes_adapters::protected_storage::ObjectProtection;
    use wes_core::flow::{FlowPolicy, Residence};
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let key = ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [17; 32]).unwrap();
    let open = || {
        TieredValues::open_protected(
            &live,
            &archive,
            Limits {
                bytes: CHUNK_BYTES * 4,
                ..Limits::default()
            },
            Durability::FileAndDirectory,
            NonZeroU64::new(8 * 1024 * 1024),
            Some(key.clone()),
        )
        .unwrap()
    };
    let mut store = open();
    let sentinel = b"synthetic-sealed-source-sentinel";
    let payload = sentinel.repeat((CHUNK_BYTES * 3) / sentinel.len() + 1);
    let mut retained = Vec::new();
    for residence in [
        None,
        Some(Residence::Retainable),
        Some(Residence::Temporary),
    ] {
        let policy = residence.map_or_else(FlowPolicy::default, |r| {
            FlowPolicy::default().confidential(r)
        });
        let shell = shell().with_provenance(wes_core::Provenance::default().with_policy(&policy));
        let owner = Arc::new(());
        let handle = store
            .begin_acquisition(payload.len() as u64, Arc::downgrade(&owner))
            .unwrap();
        for chunk in payload.chunks(CHUNK_BYTES) {
            store.append_acquisition(&handle, "payload", chunk).unwrap();
        }
        // Rejecting a full consumer does not invalidate the exact bounded source.
        let snapshot = store
            .seal_acquisition(
                &handle,
                &shell,
                &[LeafSelection {
                    port: "payload".into(),
                    fields: vec![],
                    text: false,
                }],
            )
            .unwrap();
        assert!(matches!(store.read(&handle), Err(StoreError::Limit(_))));
        let from = CHUNK_BYTES as u64 - 11;
        assert_eq!(
            store.source_range(&snapshot, from, 37).unwrap().bytes,
            payload[from as usize..from as usize + 37]
        );
        if policy.allows_retention() {
            assert!(
                store
                    .keep_with_reason(&handle, Retention::Protected)
                    .unwrap()
            );
            assert_eq!(
                store.digest(&handle).unwrap().as_ref(),
                Some(&snapshot.digest)
            );
            retained.push(snapshot);
        } else {
            assert!(matches!(
                store.keep_with_reason(&handle, Retention::Protected),
                Err(StoreError::Restricted)
            ));
            assert!(!store.is_kept(&handle).unwrap());
        }
    }
    fn check(path: &std::path::Path, sentinel: &[u8]) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check(&path, sentinel);
            } else {
                assert!(
                    !std::fs::read(&path)
                        .unwrap()
                        .windows(sentinel.len())
                        .any(|b| b == sentinel),
                    "{}",
                    path.display()
                );
            }
        }
    }
    check(root.path(), sentinel);
    drop(store);
    let store = open();
    for snapshot in retained {
        assert_eq!(
            store.digest(&snapshot.handle).unwrap().as_ref(),
            Some(&snapshot.digest)
        );
        assert_eq!(
            store.source_range(&snapshot, 7, 37).unwrap().bytes,
            payload[7..44]
        );
    }
}
#[test]
fn encrypted_record_compaction_composite_reopen_and_tampering_are_bounded() {
    use wes_adapters::protected_storage::{OVERHEAD, ObjectProtection};
    use wes_core::{
        RecordShape,
        flow::{FlowPolicy, Residence},
    };
    use wes_engine::storage::{SnapshotMember, ValueHandle};
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let key = ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [31; 32]).unwrap();
    let open = || {
        TieredValues::open_protected(
            &live,
            &archive,
            Limits {
                bytes: 8192,
                ..Limits::default()
            },
            Durability::FileAndDirectory,
            NonZeroU64::new(4 * 1024 * 1024),
            Some(key.clone()),
        )
        .unwrap()
    };
    let mut store = open();
    let body = Shape::Record(
        RecordShape::new(
            "Body",
            [
                ("code".into(), Shape::Primitive(Primitive::Int)),
                ("log".into(), Shape::Primitive(Primitive::Text)),
            ],
        )
        .unwrap(),
    );
    let shape = Shape::Record(RecordShape::new("Output", [("body".into(), body)]).unwrap());
    let value = Value::new(
        shape.clone(),
        Data::Record(indexmap::IndexMap::from([(
            "body".into(),
            Data::Record(indexmap::IndexMap::from([
                ("code".into(), Data::Int(7)),
                ("log".into(), Data::Text("".into())),
            ])),
        )])),
        wes_core::Provenance::default()
            .with_policy(&FlowPolicy::default().confidential(Residence::Retainable)),
    )
    .unwrap();
    let owner = Arc::new(());
    let handle = store
        .begin_acquisition((5 * CHUNK_BYTES) as u64, Arc::downgrade(&owner))
        .unwrap();
    for _ in 0..3 {
        store
            .append_acquisition(&handle, "discarded", &vec![b'd'; CHUNK_BYTES])
            .unwrap();
    }
    for _ in 0..2 {
        store
            .append_acquisition(&handle, "log", &vec![b'l'; CHUNK_BYTES])
            .unwrap();
    }
    let snapshot = store
        .seal_acquisition(
            &handle,
            &value,
            &[LeafSelection {
                port: "log".into(),
                fields: vec!["body".into(), "log".into()],
                text: true,
            }],
        )
        .unwrap();
    // Rewriting retained chunks must bind the new physical offsets, not the draft offsets.
    fn binary_path(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == name {
                return Some(entry.path());
            }
            if entry.file_type().unwrap().is_dir() {
                if let Some(path) = binary_path(&entry.path(), name) {
                    return Some(path);
                }
            }
        }
        None
    }
    assert_eq!(
        std::fs::metadata(binary_path(root.path(), &format!("{handle}.bin")).unwrap())
            .unwrap()
            .len(),
        (2 * (CHUNK_BYTES + OVERHEAD)) as u64
    );
    let leaf = store
        .describe(&handle, &["body".into(), "log".into()])
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .source_range(&leaf, CHUNK_BYTES as u64 - 3, 10)
            .unwrap()
            .bytes,
        vec![b'l'; 10]
    );
    assert!(matches!(store.read(&handle), Err(StoreError::Limit(_))));
    assert!(
        store
            .keep_with_reason(&handle, Retention::Protected)
            .unwrap()
    );
    let alias = ValueHandle::fresh();
    store
        .compose(
            &alias,
            &[SnapshotMember {
                name: "evidence".into(),
                handle: handle.to_string(),
                digest: snapshot.digest.clone(),
                selection: vec!["body".into()],
            }],
        )
        .unwrap();
    assert!(
        store
            .keep_with_reason(&alias, Retention::Protected)
            .unwrap()
    );
    let alias_digest = store.digest(&alias).unwrap().unwrap();
    drop(store);
    let store = open();
    assert_eq!(store.describe(&handle, &[]).unwrap().unwrap().shape, shape);
    assert_eq!(store.digest(&alias).unwrap().unwrap(), alias_digest);
    assert_eq!(
        store
            .read_selected(&alias, &["evidence".into(), "code".into()], 8192)
            .unwrap()
            .unwrap()
            .value
            .data(),
        &Data::Int(7)
    );
    let leaf = store
        .describe(&alias, &["evidence".into(), "log".into()])
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .source_range(&leaf, CHUNK_BYTES as u64 - 3, 10)
            .unwrap()
            .bytes,
        vec![b'l'; 10]
    );
    // A corrupted authenticated chunk is a failed read, never an empty successful range.
    let binary = archive.join(format!("{handle}.bin"));
    let mut bytes = std::fs::read(&binary).unwrap();
    bytes[OVERHEAD + 17] ^= 1;
    std::fs::write(&binary, bytes).unwrap();
    assert!(matches!(
        store.source_range(&leaf, 0, 32),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn large_finite_acquisition_ranges_keep_and_reopen_never_materialize_the_whole_input() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let mut store = TieredValues::open(
        &live,
        &archive,
        Limits::default(),
        Durability::FileAndDirectory,
        NonZeroU64::new(1024 * 1024 * 1024),
    )
    .unwrap();
    let owner = Arc::new(());
    let bytes = 500 * 1024 * 1024u64;
    let handle = store
        .begin_acquisition(bytes, Arc::downgrade(&owner))
        .unwrap();
    let buffer = vec![b'x'; CHUNK_BYTES];
    let mut high_water = 0;
    for _ in 0..bytes / CHUNK_BYTES as u64 {
        store
            .append_acquisition(&handle, "payload", &buffer)
            .unwrap();
        let usage = store.acquisition_usage();
        assert_eq!(usage.drafts, 1);
        assert_eq!(usage.buffer_bytes, CHUNK_BYTES);
        assert!(usage.metadata_charge <= wes_budgets::get("storage.acquisition.metadata.bytes"));
        high_water = high_water.max(usage.metadata_charge);
    }
    assert_eq!(store.acquisition_usage().payload_bytes, bytes);
    assert!(high_water < 8 * 1024 * 1024);
    assert!(
        store.read(&handle).unwrap().is_none(),
        "a draft is never a complete result"
    );
    let snapshot = store
        .seal_acquisition(
            &handle,
            &shell(),
            &[LeafSelection {
                port: "payload".into(),
                fields: vec![],
                text: false,
            }],
        )
        .unwrap();
    assert_eq!(store.acquisition_usage().drafts, 0);
    assert_eq!(snapshot.shape, Shape::Primitive(Primitive::Bytes));
    assert!(matches!(store.read(&handle), Err(StoreError::Limit(_))));
    assert!(matches!(store.encoded(&handle), Err(StoreError::Limit(_))));
    let range = store
        .source_range(&snapshot, CHUNK_BYTES as u64 - 3, 10)
        .unwrap();
    assert_eq!(range.bytes, vec![b'x'; 10]);
    assert_eq!(range.extent, bytes);
    let hold = store.hold(&handle).unwrap().unwrap();
    assert!(matches!(store.release(&handle), Err(StoreError::Limit(_))));
    assert!(
        store
            .keep_with_reason(&handle, Retention::Protected)
            .unwrap()
    );
    let digest = store.digest(&handle).unwrap().unwrap();
    drop(hold);
    drop(owner);
    drop(store);
    let store = TieredValues::open(
        &live,
        &archive,
        Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    assert_eq!(store.digest(&handle).unwrap().unwrap(), digest);
    assert_eq!(
        store.source_range(&snapshot, bytes - 7, 64).unwrap().bytes,
        vec![b'x'; 7]
    );
    assert!(store.is_kept(&handle).unwrap());
}
#[test]
fn cancelled_and_over_budget_drafts_cannot_publish_or_consume_capacity_forever() {
    let root = tempfile::tempdir().unwrap();
    let mut store = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::File,
        NonZeroU64::new(256 * 1024),
    )
    .unwrap();
    let owner = Arc::new(());
    let h = store.begin_acquisition(32, Arc::downgrade(&owner)).unwrap();
    assert!(matches!(
        store.append_acquisition(&h, "payload", &[0; 33]),
        Err(StoreError::Limit(_))
    ));
    drop(owner);
    let owner = Arc::new(());
    let next = store.begin_acquisition(32, Arc::downgrade(&owner)).unwrap();
    assert!(store.read(&h).unwrap().is_none());
    store
        .append_acquisition(&next, "payload", &[3; 32])
        .unwrap();
    let s = store
        .seal_acquisition(
            &next,
            &shell(),
            &[LeafSelection {
                port: "payload".into(),
                fields: vec![],
                text: false,
            }],
        )
        .unwrap();
    assert_eq!(store.source_range(&s, 0, 64).unwrap().bytes, vec![3; 32]);
}

#[test]
fn split_utf8_is_verified_before_publication_and_corrupt_chunks_cannot_be_kept() {
    use std::io::{Seek, SeekFrom, Write};
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let mut store = TieredValues::open(
        &live,
        &archive,
        Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let owner = Arc::new(());
    let h = store
        .begin_acquisition(CHUNK_BYTES as u64 + 2, Arc::downgrade(&owner))
        .unwrap();
    let mut first = vec![b'a'; CHUNK_BYTES - 1];
    first.push(0xe2);
    store.append_acquisition(&h, "payload", &first).unwrap();
    store
        .append_acquisition(&h, "payload", &[0x82, 0xac])
        .unwrap();
    let text = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("".into()),
        Default::default(),
    )
    .unwrap();
    let leaves = [LeafSelection {
        port: "payload".into(),
        fields: vec![],
        text: true,
    }];
    let snapshot = store.seal_acquisition(&h, &text, &leaves).unwrap();
    let loaded = store.read(&h).unwrap().unwrap();
    assert!(
        matches!(loaded.value.data(), Data::Text(s) if s.ends_with('€') && s.len() == CHUNK_BYTES + 2)
    );
    let hold = store.hold(&h).unwrap().unwrap();
    let directory = std::fs::read_dir(&live)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(".wes-transient-"))
        })
        .unwrap()
        .path();
    let mut binary = std::fs::OpenOptions::new()
        .write(true)
        .open(directory.join(format!("{h}.bin")))
        .unwrap();
    binary.seek(SeekFrom::Start(CHUNK_BYTES as u64)).unwrap();
    binary.write_all(b"z").unwrap();
    binary.sync_all().unwrap();
    assert_eq!(
        store.source_range(&snapshot, 0, 10).unwrap().bytes,
        vec![b'a'; 10],
        "a range validates only its requested chunks"
    );
    assert!(matches!(
        store.source_range(&snapshot, CHUNK_BYTES as u64, 2),
        Err(StoreError::Corrupt)
    ));
    assert!(store.keep_with_reason(&h, Retention::Protected).is_err());
    binary.seek(SeekFrom::Start(0)).unwrap();
    binary.write_all(b"z").unwrap();
    binary.sync_all().unwrap();
    assert!(matches!(
        store.source_range(&snapshot, 0, 10),
        Err(StoreError::Corrupt)
    ));
    assert!(store.keep_with_reason(&h, Retention::Protected).is_err());
    assert!(!store.is_kept(&h).unwrap());
    assert!(!archive.join(format!("{h}.json")).exists());
    drop(hold);

    let bad = store.begin_acquisition(1, Arc::downgrade(&owner)).unwrap();
    store.append_acquisition(&bad, "payload", &[0xff]).unwrap();
    assert!(store.seal_acquisition(&bad, &text, &leaves).is_err());
    assert!(store.read(&bad).unwrap().is_none());
    store.abort_acquisition(&bad).unwrap();
}

#[test]
fn retained_record_holds_protect_shared_members_and_release_when_its_last_reader_finishes() {
    use wes_engine::storage::{SnapshotMember, ValueHandle};
    let root = tempfile::tempdir().unwrap();
    let mut store = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let source = Value::new(
        Shape::Primitive(Primitive::Bytes),
        Data::Bytes(b"captured".as_slice().into()),
        Default::default(),
    )
    .unwrap();
    let handle = store.store(&source).unwrap();
    store
        .keep_with_reason(&handle, Retention::Protected)
        .unwrap();
    let digest = store.digest(&handle).unwrap().unwrap();
    let alias = ValueHandle::fresh();
    let member = |name: &str| SnapshotMember {
        name: name.into(),
        handle: handle.to_string(),
        digest: digest.clone(),
        selection: vec![],
    };
    store
        .compose(&alias, &[member("first"), member("second")])
        .unwrap();
    let hold = store.hold(&alias).unwrap().unwrap();
    assert!(matches!(store.release(&handle), Err(StoreError::Limit(_))));
    assert!(matches!(store.release(&alias), Err(StoreError::Limit(_))));
    drop(hold);
    assert!(store.release(&handle).unwrap());
    assert!(store.describe(&alias, &["first".into()]).is_err());
}

#[test]
fn nested_record_leaf_ranges_and_keep_preserve_the_full_captured_type() {
    use wes_core::RecordShape;
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let limits = Limits {
        bytes: 8192,
        ..Limits::default()
    };
    let mut store =
        TieredValues::open(&live, &archive, limits, Durability::FileAndDirectory, None).unwrap();
    let body = Shape::Record(
        RecordShape::new(
            "Body",
            [
                ("code".into(), Shape::Primitive(Primitive::Int)),
                ("log".into(), Shape::Primitive(Primitive::Text)),
            ],
        )
        .unwrap(),
    );
    let shape = Shape::Record(RecordShape::new("Output", [("body".into(), body)]).unwrap());
    let value = Value::new(
        shape.clone(),
        Data::Record(indexmap::IndexMap::from([(
            "body".into(),
            Data::Record(indexmap::IndexMap::from([
                ("code".into(), Data::Int(7)),
                ("log".into(), Data::Text("".into())),
            ])),
        )])),
        Default::default(),
    )
    .unwrap();
    let owner = Arc::new(());
    let handle = store
        .begin_acquisition((2 * CHUNK_BYTES) as u64, Arc::downgrade(&owner))
        .unwrap();
    let bytes = vec![b'x'; CHUNK_BYTES];
    for _ in 0..2 {
        store.append_acquisition(&handle, "log", &bytes).unwrap();
    }
    let snapshot = store
        .seal_acquisition(
            &handle,
            &value,
            &[LeafSelection {
                port: "log".into(),
                fields: vec!["body".into(), "log".into()],
                text: true,
            }],
        )
        .unwrap();
    assert_eq!(snapshot.shape, shape);
    assert!(matches!(store.read(&handle), Err(StoreError::Limit(_))));
    let fields = vec!["body".into(), "log".into()];
    let leaf = store.describe(&handle, &fields).unwrap().unwrap();
    assert_eq!(
        store
            .source_range(&leaf, CHUNK_BYTES as u64 - 3, 10)
            .unwrap()
            .bytes,
        vec![b'x'; 10]
    );
    let hold = store.hold(&handle).unwrap().unwrap();
    assert!(
        store
            .keep_with_reason(&handle, Retention::Protected)
            .unwrap()
    );
    drop(hold);
    drop(owner);
    drop(store);
    let store =
        TieredValues::open(&live, &archive, limits, Durability::FileAndDirectory, None).unwrap();
    let reopened = store.describe(&handle, &[]).unwrap().unwrap();
    assert_eq!(reopened.digest, snapshot.digest);
    assert_eq!(reopened.shape, shape);
    assert_eq!(
        store
            .read_selected(&handle, &["body".into(), "code".into()], 8192)
            .unwrap()
            .unwrap()
            .value
            .data(),
        &Data::Int(7)
    );
    let leaf = store.describe(&handle, &fields).unwrap().unwrap();
    assert_eq!(
        store
            .source_range(&leaf, 2 * CHUNK_BYTES as u64 - 5, 64)
            .unwrap()
            .bytes,
        vec![b'x'; 5]
    );
    // Physically complete bytes do not make a noncanonical index valid. Accepting these
    // many short chunks would defeat the at-most-two-chunks range working-set bound.
    use sha2::{Digest, Sha256};
    let path = archive.join(format!("{handle}.json"));
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let digest = format!("{:x}", Sha256::digest(vec![b'x'; 4096]));
    manifest["leaves"][0]["chunks"] = serde_json::Value::Array(
        (0..32)
            .map(|index| serde_json::json!({"offset":index * 4096,"bytes":4096,"digest":digest}))
            .collect(),
    );
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(matches!(
        store.describe(&handle, &[]),
        Err(StoreError::Corrupt)
    ));
}
