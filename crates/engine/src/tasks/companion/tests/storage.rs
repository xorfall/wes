use super::*;
use crate::storage::{
    LoadedValue, StoreError, StoredSnapshot, ValueHandle, ValueLease, ValueStore,
};
use std::{collections::HashMap, sync::Weak};

/// Synthetic native backend: these tests exercise owner handoff, not disk encryption.
#[derive(Default)]
struct Store {
    values: HashMap<ValueHandle, Value>,
    drafts: HashMap<ValueHandle, Vec<u8>>,
}
impl ValueStore for Store {
    fn read_policy(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        Ok(self
            .values
            .get(handle)
            .map(|v| v.provenance().policy().clone()))
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        let handle = ValueHandle::fresh();
        self.values.insert(handle.clone(), value.clone());
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        Ok(self
            .values
            .get(handle)
            .cloned()
            .map(|value| LoadedValue { value }))
    }
    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        Ok(self
            .values
            .contains_key(handle)
            .then(|| format!("sha256:{}", "a".repeat(64))))
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn keep(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn is_kept(&self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(self.values.contains_key(handle).then_some(256))
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.values.remove(handle).is_some())
    }
    fn begin_acquisition(&mut self, _: u64, _: Weak<()>) -> Result<ValueHandle, StoreError> {
        let handle = ValueHandle::fresh();
        self.drafts.insert(handle.clone(), vec![]);
        Ok(handle)
    }
    fn append_acquisition(
        &mut self,
        handle: &ValueHandle,
        _: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.drafts
            .get_mut(handle)
            .ok_or(StoreError::MissingValue)?
            .extend_from_slice(bytes);
        Ok(())
    }
    fn seal_acquisition(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        leaves: &[crate::storage::LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        let bytes = self.drafts.remove(handle).ok_or(StoreError::MissingValue)?;
        let mut data = shell.data().clone();
        for selection in leaves {
            let mut field = &mut data;
            for name in &selection.fields {
                let Data::Record(record) = field else {
                    return Err(StoreError::Conflict);
                };
                field = record.get_mut(name).ok_or(StoreError::Conflict)?;
            }
            *field = Data::Bytes(bytes.clone().into());
        }
        let value = Value::new(shell.shape().clone(), data, shell.provenance().clone())
            .unwrap()
            .with_metadata(shell.metadata().cloned());
        let snapshot =
            StoredSnapshot::capture(handle.clone(), format!("sha256:{}", "a".repeat(64)), &value)
                .ok_or(StoreError::Conflict)?;
        self.values.insert(handle.clone(), value);
        Ok(snapshot)
    }
    fn abort_acquisition(&mut self, handle: &ValueHandle) -> Result<(), StoreError> {
        self.drafts.remove(handle);
        Ok(())
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        if !fields.is_empty() {
            return Ok(None);
        }
        self.values
            .get(handle)
            .map(|value| {
                StoredSnapshot::capture(handle.clone(), format!("sha256:{}", "a".repeat(64)), value)
                    .ok_or(StoreError::Conflict)
            })
            .transpose()
    }
    fn hold(&mut self, handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        Ok(self.values.get(handle).cloned().map(ValueLease::retain))
    }
}

#[tokio::test]
async fn stored_business_result_is_published_early_and_original_run_raw_is_accessible_only_after_physical_join()
 {
    let (worker, writer) =
        crate::storage::spawn_store(Store::default(), Default::default()).unwrap();
    let scoped = worker.transient_scope().unwrap();
    let mut f = Fixture::new(true, true);
    let running = Running::start(f.task(Duration::ZERO), Some(scoped.clone())).await;
    let source = next(&mut f.sources).await;
    f.ready(&source);
    let business = next(&mut f.businesses).await;
    assert_eq!(business.run, source.run);
    business.finish.send(Ok(integer(7))).unwrap();
    let control = next(&mut f.controls).await;
    running.handle.wait_idle().await.unwrap();
    let before = running.snapshot().await;
    let stored = before.stored_values[&running.node].snapshot.clone();
    let (loaded, _hold) = scoped.materialize(stored.clone()).await.unwrap();
    assert_eq!(loaded.value.data(), integer(7).data());
    assert!(
        running
            .owner
            .recorded_source(&running.node, source.run.id().as_str())
            .is_err()
    );
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    source.finish.send(Ok(())).unwrap();
    control.token.cancelled().await;
    assert!(
        running
            .owner
            .recorded_source(&running.node, source.run.id().as_str())
            .is_err(),
        "a finalized raw draft is not the source/controller join receipt"
    );
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .ok()
        .unwrap();
    running.joined().await;
    let after = running.snapshot().await;
    assert_eq!(after.stored_values[&running.node].snapshot, stored);
    assert!(
        running
            .owner
            .source(&running.node, source.run.id().as_str())
            .is_err()
    );
    assert!(
        running
            .owner
            .recorded_source(&running.node, "another-run")
            .is_err()
    );
    let recording = running
        .owner
        .recorded_source(&running.node, source.run.id().as_str())
        .unwrap();
    let (loaded, _raw_hold) = scoped
        .materialize(recording.output.snapshot.clone().unwrap())
        .await
        .unwrap();
    let Data::Record(record) = loaded.value.data() else {
        panic!("recording")
    };
    assert_eq!(
        record["run"],
        Data::Text(source.run.id().to_string().into())
    );
    assert_eq!(record["acceptedBytes"], Data::Text("17".into()));
    let Data::Record(raw) = &record["value"] else {
        panic!("raw")
    };
    assert_eq!(
        raw["raw"],
        Data::Bytes(b"synthetic-packets".to_vec().into())
    );
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    running.close().await;
    scoped.close_transient_scope().await.unwrap();
    worker.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn mandatory_raw_reservation_failure_prevents_external_capture_and_business_entry() {
    let f = Fixture::new(true, false);
    let running = Running::start(f.task(Duration::ZERO), None).await;
    running.handle.wait_idle().await.unwrap();
    running.joined().await;
    assert!(running.snapshot().await.errors.contains_key(&running.node));
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    running.close().await;
}

#[tokio::test]
async fn predispatch_capture_refusal_keeps_real_business_input_restrictions() {
    let f = Fixture::new(true, false);
    let mut task = f.task(Duration::ZERO);
    let BoundTask::Companion(companion) = &mut task else {
        unreachable!()
    };
    let private = integer(7).with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    companion.call = companion
        .call
        .clone()
        .with_inputs([("number".into(), crate::plan::Input::Literal(private))].into());
    let running = Running::start(task, None).await;
    running.handle.wait_idle().await.unwrap();
    running.joined().await;
    assert!(
        running.snapshot().await.errors[&running.node]
            .policy()
            .is_private()
    );
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    running.close().await;
}

#[tokio::test]
async fn private_capture_refuses_mandatory_disk_recording_before_external_capture_or_business_entry()
 {
    let (worker, writer) =
        crate::storage::spawn_store(Store::default(), Default::default()).unwrap();
    let scoped = worker.transient_scope().unwrap();
    let f = Fixture::new(true, false);
    let mut task = f.task(Duration::ZERO);
    let BoundTask::Companion(companion) = &mut task else {
        unreachable!()
    };
    let private = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("synthetic".into()),
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    companion.source = companion
        .source
        .clone()
        .with_inputs([("note".into(), crate::plan::Input::Literal(private))].into());
    let running = Running::start(task, Some(scoped.clone())).await;
    running.handle.wait_idle().await.unwrap();
    running.joined().await;
    assert!(
        running.snapshot().await.errors[&running.node]
            .policy()
            .is_private()
    );
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    running.close().await;
    scoped.close_transient_scope().await.unwrap();
    worker.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
