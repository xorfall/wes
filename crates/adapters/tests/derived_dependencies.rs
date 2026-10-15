//! Native dependency witnesses preserve original storage without public handle authority.
use std::sync::Arc;
use wes_adapters::{
    codec::Limits,
    protected_storage::ObjectProtection,
    storage::{Durability, TieredValues},
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    flow::{FlowPolicy, Residence},
};
use wes_engine::storage::{
    CHUNK_BYTES, LeafSelection, SnapshotDependency, StoreError, StoreWorkerLimits, ValueStore,
    spawn_store,
};

#[tokio::test]
async fn owned_derivation_keeps_exact_raw_closure_and_reopens_with_original_membership() {
    for residence in [Residence::Retainable, Residence::Temporary] {
        let home = tempfile::tempdir().unwrap();
        let key = ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [74; 32]).unwrap();
        let open = || {
            TieredValues::open_protected(
                &home.path().join("live"),
                &home.path().join("archive"),
                Limits {
                    bytes: 4 * CHUNK_BYTES,
                    ..Default::default()
                },
                Durability::FileAndDirectory,
                None,
                Some(key.clone()),
            )
            .unwrap()
        };
        let mut backend = open();
        let policy = FlowPolicy::default().confidential(residence);
        let provenance = Provenance::default().with_policy(&policy);
        let shell = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(vec![].into()),
            provenance.clone(),
        )
        .unwrap();
        let owner = Arc::new(());
        let raw = backend
            .begin_acquisition(8 * CHUNK_BYTES as u64, Arc::downgrade(&owner))
            .unwrap();
        for n in 0..8 {
            backend
                .append_acquisition(&raw, "raw", &vec![n; CHUNK_BYTES])
                .unwrap();
        }
        let original = backend
            .seal_acquisition(
                &raw,
                &shell,
                &[LeafSelection {
                    port: "raw".into(),
                    fields: vec![],
                    text: false,
                }],
            )
            .unwrap();
        assert!(matches!(backend.read(&raw), Err(StoreError::Limit(_))));
        let shell = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("synthetic analysis".into()),
            provenance,
        )
        .unwrap();
        let derived = backend
            .begin_acquisition(1, Arc::downgrade(&owner))
            .unwrap();
        let dependency = SnapshotDependency {
            name: "input".into(),
            handle: raw.to_string(),
            digest: original.digest.clone(),
            selection: vec![],
        };
        let snapshot = backend
            .seal_acquisition_with_dependencies(&derived, &shell, &[], &[dependency.clone()])
            .unwrap();
        assert!(!backend.automatic_retention_allowed(&derived).unwrap());
        let wrong = backend
            .begin_acquisition(1, Arc::downgrade(&owner))
            .unwrap();
        let mut forged = dependency.clone();
        forged.digest = format!("sha256:{}", "0".repeat(64));
        assert!(
            backend
                .seal_acquisition_with_dependencies(&wrong, &shell, &[], &[forged])
                .is_err()
        );
        backend.abort_acquisition(&wrong).unwrap();
        let wrong = backend
            .begin_acquisition(1, Arc::downgrade(&owner))
            .unwrap();
        let mut missing_selection = dependency.clone();
        missing_selection.selection = vec!["missing".into()];
        assert!(
            backend
                .seal_acquisition_with_dependencies(&wrong, &shell, &[], &[missing_selection])
                .is_err()
        );
        backend.abort_acquisition(&wrong).unwrap();
        let wrong = backend
            .begin_acquisition(1, Arc::downgrade(&owner))
            .unwrap();
        let public = Value::new(
            shell.shape().clone(),
            shell.data().clone(),
            Default::default(),
        )
        .unwrap();
        assert!(
            backend
                .seal_acquisition_with_dependencies(&wrong, &public, &[], &[dependency])
                .is_err()
        );
        backend.abort_acquisition(&wrong).unwrap();
        let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
        let range = worker
            .source_range_dependency(snapshot.clone(), "input".into(), CHUNK_BYTES as u64 - 2, 4)
            .await
            .unwrap();
        assert_eq!(range.bytes, [0, 0, 1, 1]);
        assert_eq!(range.extent, 8 * CHUNK_BYTES as u64);
        let lease = worker.hold(derived.clone()).await.unwrap().unwrap();
        assert!(matches!(
            worker.release(raw.clone()).await,
            Err(StoreError::Limit(_))
        ));
        drop(lease);
        if residence == Residence::Temporary {
            assert!(matches!(
                worker.keep(derived.clone()).await,
                Err(StoreError::Restricted)
            ));
            assert!(!worker.is_kept(raw).await.unwrap());
            assert!(!worker.is_kept(derived).await.unwrap());
        } else {
            assert!(worker.keep(derived.clone()).await.unwrap());
            assert!(worker.is_kept(raw.clone()).await.unwrap());
            assert_eq!(worker.shutdown().await.unwrap().failed, 1);
            task.join().await.unwrap();
            drop(worker);
            let (worker, task) = spawn_store(open(), StoreWorkerLimits::default()).unwrap();
            assert!(worker.is_kept(derived.clone()).await.unwrap());
            assert!(worker.is_kept(raw).await.unwrap());
            let reopened = worker.describe(derived, vec![]).await.unwrap().unwrap();
            assert_eq!(reopened.digest, snapshot.digest);
            assert_eq!(
                worker
                    .source_range_dependency(reopened, "input".into(), CHUNK_BYTES as u64 - 2, 4)
                    .await
                    .unwrap()
                    .bytes,
                [0, 0, 1, 1]
            );
            assert_eq!(worker.shutdown().await.unwrap().failed, 0);
            task.join().await.unwrap();
            continue;
        }
        // Only the deliberately failed held release is accounted; policy refusal adds none.
        assert_eq!(worker.shutdown().await.unwrap().failed, 1);
        task.join().await.unwrap();
    }
}
