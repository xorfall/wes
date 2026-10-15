//! Native queries over real encrypted cold evidence; all homes and bytes are synthetic.
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};
use wes_adapters::{
    codec::Limits,
    protected_storage::ObjectProtection,
    storage::{Durability, TieredValues},
};
use wes_core::{
    Data, Primitive, Provenance, RecordShape, Shape, Value,
    flow::{FlowPolicy, Residence},
};
use wes_engine::{
    history::Persistence,
    providers::LocalScope,
    runtime::{Effect, Outcome},
    session::{self, RecordingMode, SessionStorage},
    source::SourceInput,
    storage::{
        AutoKeep, CHUNK_BYTES, LeafSelection, PublicationPolicy, Retention, SnapshotDescriptor,
        StoreError, StoreWorkerLimits, StoredOutput, ValueStore, spawn_store,
    },
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::{Preparation, Workspace},
};
use wes_language::{SourceText, parse};

struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("byte queries do not read package files")
    }
}

#[tokio::test]
async fn native_byte_read_of_large_encrypted_selected_source_never_materializes_or_replays_it() {
    let home = tempfile::tempdir().unwrap();
    let key = ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [93; 32]).unwrap();
    // A whole payload is deliberately much larger than the ordinary decoder's admission.
    let mut backend = TieredValues::open_protected(
        &home.path().join("live"),
        &home.path().join("archive"),
        Limits {
            bytes: 2 * CHUNK_BYTES,
            ..Limits::default()
        },
        Durability::FileAndDirectory,
        NonZeroU64::new(32 * 1024 * 1024),
        Some(key),
    )
    .unwrap();
    let policy = FlowPolicy::default().confidential(Residence::Retainable);
    let shell = Value::new(
        Shape::Record(
            RecordShape::new(
                "SyntheticEvidence",
                [
                    ("raw".into(), Shape::Primitive(Primitive::Bytes)),
                    ("label".into(), Shape::Primitive(Primitive::Text)),
                ],
            )
            .unwrap(),
        ),
        Data::Record(
            [
                ("raw".into(), Data::Bytes(vec![].into())),
                ("label".into(), Data::Text("synthetic raw evidence".into())),
            ]
            .into(),
        ),
        Provenance::default().with_policy(&policy),
    )
    .unwrap();
    let extent = 16 * 1024 * 1024u64;
    let owner = Arc::new(());
    let handle = backend
        .begin_acquisition(extent, Arc::downgrade(&owner))
        .unwrap();
    let chunk: Vec<u8> = (0..CHUNK_BYTES).map(|n| (n % 251) as u8).collect();
    for _ in 0..extent / CHUNK_BYTES as u64 {
        backend
            .append_acquisition(&handle, "payload", &chunk)
            .unwrap();
    }
    let snapshot = backend
        .seal_acquisition(
            &handle,
            &shell,
            &[LeafSelection {
                port: "payload".into(),
                fields: vec!["raw".into()],
                text: false,
            }],
        )
        .unwrap();
    assert!(matches!(backend.read(&handle), Err(StoreError::Limit(_))));
    let physical_bytes = backend.size(&handle).unwrap().unwrap();
    let hold = backend.hold(&handle).unwrap().unwrap();
    let (worker, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();

    let mut workspace = Workspace::local(LocalScope::new("fixture").unwrap());
    let source = parse(&SourceText::new(
        "fixture",
        ":calc { return 0; } > raw_source",
    ));
    assert!(source.diagnostics.is_empty());
    let Preparation::Change(change) = workspace.prepare(&source.script.statements[0]).unwrap()
    else {
        panic!("native source node")
    };
    let original = workspace.commit(change).unwrap().node.unwrap();
    let run = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    workspace.enter(&run);
    workspace.complete(
        &run,
        Outcome::Stored(StoredOutput {
            handle: handle.clone(),
            bytes: physical_bytes,
            kept: false,
            retention: Retention::Temporary,
            retained_persistence: Persistence::FileAndDirectorySynced,
            requires_ownership_record: true,
            snapshot: Some(snapshot.clone()),
        }),
        Duration::ZERO,
    );
    let original_run = workspace
        .runtime()
        .value_run(&original)
        .unwrap()
        .to_string();
    assert!(workspace.runtime().value_of(&original).is_none());
    let (session, session_task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    let read = session
        .submit(
            SourceInput::new(
                "slice".into(),
                ":read bytes:$raw_source from:\"65533\" limit:10 select:raw > slice".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.nodes.len(), 1, "{:?}", read.diagnostics);
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let image = session.snapshot().await.unwrap();
    let value = image
        .execution
        .values
        .get(&read.nodes[0])
        .unwrap_or_else(|| panic!("byte read failed: {:?}", image.execution.errors));
    let Data::Record(fields) = value.data() else {
        panic!("ByteRange")
    };
    let expected = [chunk[CHUNK_BYTES - 3..].to_vec(), chunk[..7].to_vec()].concat();
    assert_eq!(fields["data"], Data::Bytes(expected.into()));
    assert_eq!(fields["extent"], Data::Text(extent.to_string().into()));
    assert_eq!(fields["next"], Data::Text("65543".into()));
    assert_eq!(
        fields["digest"],
        Data::Option(Some(Box::new(Data::Text(snapshot.digest.clone().into()))))
    );
    assert_eq!(
        fields["handle"],
        Data::Option(Some(Box::new(Data::Text(handle.to_string().into()))))
    );
    assert_eq!(
        fields["run"],
        Data::Option(Some(Box::new(Data::Text(original_run.into()))))
    );
    assert!(value.provenance().policy().is_confidential());
    assert_eq!(
        value.provenance().policy().residence(),
        Residence::Retainable
    );
    assert!(
        !image.execution.values.contains_key(&original),
        "source stays cold"
    );
    session.shutdown().await.unwrap();
    session_task.join().await.unwrap();

    for (field, from) in [("missing", 0), ("raw", extent + 1)] {
        assert!(matches!(
            worker
                .source_range_selected(snapshot.clone(), vec![field.into()], from, 10)
                .await,
            Err(StoreError::SourceRange(_))
        ));
    }
    assert_eq!(
        worker.flush().await.unwrap().failed,
        0,
        "invalid field/coordinates are logical read refusals"
    );

    let mut forged: SnapshotDescriptor = (*snapshot).clone();
    forged.digest = "different-captured-digest".into();
    assert!(matches!(
        worker
            .source_range_selected(
                wes_engine::storage::StoredSnapshot::new(forged),
                vec!["missing".into()],
                0,
                10
            )
            .await,
        Err(StoreError::Conflict)
    ));
    drop(hold);
    worker.shutdown().await.unwrap();
    store_task.join().await.unwrap();
}

#[tokio::test]
async fn native_byte_ranges_cannot_escape_or_renew_their_transient_storage_scope() {
    let home = tempfile::tempdir().unwrap();
    let backend = TieredValues::open(
        &home.path().join("live"),
        &home.path().join("archive"),
        Limits::default(),
        Durability::File,
        NonZeroU64::new(1024 * 1024),
    )
    .unwrap();
    let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let scoped = worker.transient_scope().unwrap();
    let value = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("scoped original bytes".into()),
        Provenance::default(),
    )
    .unwrap();
    let output = scoped
        .publish(value, PublicationPolicy::Temporary)
        .await
        .unwrap();
    let snapshot = scoped
        .describe(output.handle.clone(), vec![])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scoped
            .source_range(snapshot.clone(), 7, 8)
            .await
            .unwrap()
            .bytes,
        b"original"
    );
    assert!(matches!(
        worker.source_range(snapshot.clone(), 0, 4).await,
        Err(StoreError::ScopeUnavailable)
    ));
    scoped.close_transient_scope().await.unwrap();
    assert!(matches!(
        scoped.source_range(snapshot, 0, 4).await,
        Err(StoreError::Closed)
    ));
    let drain = worker.shutdown().await.unwrap();
    assert_eq!(drain.failed, 0);
    task.join().await.unwrap();
}
