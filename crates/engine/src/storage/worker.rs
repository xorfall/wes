//! Serial storage ownership off the runtime loop. Receipt cancellation does not retract admitted I/O.
use super::datasets::{
    DatasetAppend, DatasetCreate, DatasetInfo, DatasetPage, DatasetStorage, PageRequest,
};
use super::{
    EvictionBatch, LoadedValue, Retention, RetentionUsage, StoreError, ValueHandle, ValueStore,
};
use crate::value_size::value_charge;
use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
    thread,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use wes_core::Value;
mod owner;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug)]
pub struct StoreWorkerLimits {
    pub operations: NonZeroUsize,
    /// Conservative payload/encoding charge, not RSS. Reply buffers retain credit until consumed
    /// or dropped. Inputs still waiting for admission and values returned to callers are external.
    pub bytes: NonZeroU32,
}
/// Applied only to a finite, non-interactive output by the session. Streaming windows and
/// interactive transcripts must select Never, regardless of their current encoded size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoKeep {
    Never,
    UpToBytes(u64),
}
impl Default for AutoKeep {
    fn default() -> Self {
        Self::UpToBytes(wes_budgets::get("storage.keep.bytes"))
    }
}
/// Captured intent for one publication, independent of the session's automatic preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationPolicy {
    Temporary,
    AutomaticUpToBytes(u64),
    Protected,
}
impl From<AutoKeep> for PublicationPolicy {
    fn from(policy: AutoKeep) -> Self {
        match policy {
            AutoKeep::Never => Self::Temporary,
            AutoKeep::UpToBytes(limit) => Self::AutomaticUpToBytes(limit),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOutput {
    pub handle: ValueHandle,
    /// Exact stored encoding size, not display text length or memory charge.
    pub bytes: u64,
    pub kept: bool,
    pub retention: Retention,
    pub retained_persistence: crate::history::Persistence,
}
impl Default for StoreWorkerLimits {
    fn default() -> Self {
        Self {
            operations: NonZeroUsize::new(wes_budgets::get("storage.operations") as _).unwrap(),
            bytes: NonZeroU32::new(wes_budgets::get("storage.bytes") as _).unwrap(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreDrain {
    pub attempted: u64,
    pub failed: u64,
}
#[derive(Clone)]
pub struct StoreWorker {
    sender: mpsc::Sender<Request>,
    budget: Arc<Semaphore>,
    limits: StoreWorkerLimits,
    dataset_read_charge: Option<u32>,
    confidential_values: bool,
    confidential_datasets: bool,
    dataset_access: tokio::sync::watch::Receiver<super::datasets::DatasetAccess>,
    dataset_changes: Option<tokio::sync::watch::Receiver<crate::storage::datasets::DatasetChanges>>,
}
pub struct StoreWorkerTask {
    thread: thread::JoinHandle<()>,
    stopped: oneshot::Receiver<()>,
}
struct WorkspacePublication {
    worker: StoreWorker,
    runtime: tokio::runtime::Handle,
}
impl crate::history::RetainedDatasetPublication for WorkspacePublication {
    fn protect(
        &self,
        generation: &str,
        handles: &[ValueHandle],
    ) -> Result<Vec<ValueHandle>, crate::history::RecordError> {
        if handles.len() > 100_000 {
            return Err(crate::history::RecordError::Limit(
                "workspace dataset references",
            ));
        }
        self.runtime
            .block_on(
                self.worker
                    .workspace_roots(generation.into(), Some(handles.to_vec())),
            )
            .map_err(|e| {
                crate::history::RecordError::backend("protecting workspace datasets", false, e)
            })
    }
    fn retire(&self, generation: &str) -> Result<(), crate::history::RecordError> {
        self.runtime
            .block_on(self.worker.workspace_roots(generation.into(), None))
            .map(|_| ())
            .map_err(|e| {
                crate::history::RecordError::backend("retiring workspace datasets", false, e)
            })
    }
}
struct Credited<T> {
    result: Result<T, StoreError>,
    credit: OwnedSemaphorePermit,
}
pub struct PendingStore<T>(oneshot::Receiver<Credited<T>>);
impl<T> PendingStore<T> {
    /// Dropping this future/receipt never cancels an admitted filesystem operation.
    pub async fn wait(self) -> Result<T, StoreError> {
        let Credited { result, credit } = self.0.await.map_err(|_| StoreError::Closed)?;
        drop(credit);
        result
    }
}
// Type erasure is private to the mailbox. The public port exposes only ValueStore operations,
// never arbitrary closures or mutable access to the store from another thread.
trait Job: Send {
    fn perform(self: Box<Self>, store: &mut dyn OwnedStorage) -> bool;
}
trait OwnedStorage {
    fn validate_read(&self, handle: &ValueHandle, captured: &Value) -> Result<bool, StoreError>;
    fn refresh_access(&self);
    fn values(&mut self) -> &mut dyn ValueStore;
    fn retained_value_exists(&self, handle: &ValueHandle) -> Result<bool, StoreError>;
    fn datasets(&self) -> Result<&dyn DatasetStorage, StoreError>;
    fn datasets_mut(&mut self) -> Result<&mut dyn DatasetStorage, StoreError>;
    fn captured_analysis(
        &mut self,
        run: &str,
        limit: u32,
    ) -> Result<super::datasets::DatasetContinuation, StoreError> {
        let reference = self.datasets()?.continuation(run)?;
        let info = self.datasets()?.inspect(&reference)?;
        let checkpoint = self
            .datasets()?
            .checkpoint(&reference)?
            .ok_or(StoreError::DatasetMissing)?;
        self.validate_checkpoint(&checkpoint, &info.policy, true, false)?;
        let source = self
            .values()
            .read(&ValueHandle::new(&checkpoint.source.handle)?)?
            .ok_or(StoreError::MissingValue)?
            .value;
        let source = source.with_provenance(source.provenance().clone().with_policy(&info.policy));
        value_charge(&source, u64::from(limit) / 2)
            .ok_or(StoreError::Limit("checkpoint source"))?;
        let status = self.datasets()?.analysis_status(&reference)?;
        Ok(super::datasets::DatasetContinuation {
            status,
            reference,
            checkpoint,
            source,
        })
    }
    fn validate_checkpoint(
        &self,
        checkpoint: &super::datasets::AnalysisCheckpoint,
        policy: &wes_core::flow::FlowPolicy,
        validate_source: bool,
        initial_admission: bool,
    ) -> Result<(), StoreError>;
}
struct Owner<V> {
    values: super::private::PolicyValues<V>,
    datasets: Option<Box<dyn DatasetStorage>>,
    pending_evictions: std::collections::VecDeque<ValueHandle>,
    evictions_more: bool,
    dataset_access: tokio::sync::watch::Sender<super::datasets::DatasetAccess>,
}
impl<V: ValueStore> OwnedStorage for Owner<V> {
    fn validate_read(&self, handle: &ValueHandle, captured: &Value) -> Result<bool, StoreError> {
        self.validate_prefixes(captured)?;
        // A ValueHandle identifies immutable bytes. Revalidate existence and
        // current read gates without decoding the same ordinary value again.
        Ok(self.values.size(handle)?.is_some())
    }
    fn refresh_access(&self) {
        let access = match self
            .datasets
            .as_ref()
            .map(|port| port.read_access())
            .transpose()
        {
            Ok(withdrawn) => super::datasets::DatasetAccess {
                withdrawn: withdrawn.unwrap_or_default(),
                closed: false,
            },
            Err(_) => super::datasets::DatasetAccess {
                withdrawn: Default::default(),
                closed: true,
            },
        };
        self.dataset_access.send_if_modified(|previous| {
            if previous == &access {
                false
            } else {
                *previous = access;
                true
            }
        });
    }
    fn validate_checkpoint(
        &self,
        checkpoint: &super::datasets::AnalysisCheckpoint,
        policy: &wes_core::flow::FlowPolicy,
        validate_source: bool,
        initial_admission: bool,
    ) -> Result<(), StoreError> {
        use sha2::{Digest, Sha256};
        if policy.join(checkpoint.state.provenance().policy()) != *policy
            || policy.join(checkpoint.context.provenance().policy()) != *policy
            || checkpoint.state.provenance().policy().is_private()
            || checkpoint.state.provenance().policy().is_unknown()
            || checkpoint.context.provenance().policy().is_private()
            || checkpoint.context.provenance().policy().is_unknown()
            || checkpoint
                .state
                .provenance()
                .policy()
                .origins()
                .iter()
                .chain(checkpoint.context.provenance().policy().origins())
                .any(|o| !policy.origins().contains(o))
            || checkpoint
                .state
                .provenance()
                .policy()
                .dataset_reads()
                .iter()
                .chain(checkpoint.context.provenance().policy().dataset_reads())
                .any(|o| !policy.dataset_reads().contains(o))
        {
            return Err(StoreError::Restricted);
        }
        if !checkpoint.state.data().is_inline() || !checkpoint.context.data().is_inline() {
            return Err(StoreError::NonMaterialized);
        }
        if let Some(followed) = &checkpoint.followed_source {
            let info = self.datasets()?.inspect(&followed.prefix)?;
            if policy.join(&info.policy) != *policy
                || info.recording.as_ref().map(|r| (&r.run, &r.epoch, r.first))
                    != Some((&followed.run, &followed.epoch, followed.first))
                || info
                    .policy
                    .origins()
                    .iter()
                    .any(|o| !policy.origins().contains(o))
                || info
                    .policy
                    .dataset_reads()
                    .iter()
                    .any(|o| !policy.dataset_reads().contains(o))
            {
                return Err(StoreError::Restricted);
            }
        }
        if !validate_source {
            return Ok(());
        }
        let handle = ValueHandle::new(&checkpoint.source.handle)?;
        if !self.values.is_kept(&handle)? {
            return Err(StoreError::MissingValue);
        }
        let loaded = self.values.read(&handle)?.ok_or(StoreError::MissingValue)?;
        let source_policy = loaded.value.provenance().policy();
        if let Some(followed) = &checkpoint.followed_source {
            let wes_core::Data::Dataset(initial) = loaded.value.data() else {
                return Err(StoreError::Conflict);
            };
            if initial.store() != followed.prefix.store()
                || initial.dataset() != followed.prefix.dataset()
                || initial.schema_digest() != followed.prefix.schema_digest()
                || initial.authorization_generation() != followed.prefix.authorization_generation()
                || initial.generation() > followed.prefix.generation()
                || initial.records() > followed.prefix.records()
            {
                return Err(StoreError::Conflict);
            }
            // Create anchors the immutable capture exactly. Successor checks preserve
            // ancestry thereafter; resume never walks back through the entire recording.
            if initial_admission && initial.as_ref() != &followed.prefix {
                return Err(StoreError::Conflict);
            }
        }
        if policy.join(source_policy) != *policy
            || source_policy.is_private()
            || source_policy.is_unknown()
            || source_policy
                .origins()
                .iter()
                .any(|o| !policy.origins().contains(o))
            || source_policy
                .dataset_reads()
                .iter()
                .any(|o| !policy.dataset_reads().contains(o))
        {
            return Err(StoreError::Restricted);
        }
        let bytes = self
            .values
            .encoded(&handle)?
            .ok_or(StoreError::MissingValue)?;
        if bytes.len() as u64 != checkpoint.source.bytes
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != checkpoint.source.digest
        {
            return Err(StoreError::Conflict);
        }
        Ok(())
    }
    fn values(&mut self) -> &mut dyn ValueStore {
        self
    }
    fn retained_value_exists(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.values.is_kept(handle)
    }
    fn datasets(&self) -> Result<&dyn DatasetStorage, StoreError> {
        self.datasets
            .as_deref()
            .ok_or(StoreError::DatasetUnavailable)
    }
    fn datasets_mut(&mut self) -> Result<&mut dyn DatasetStorage, StoreError> {
        match self.datasets.as_mut() {
            Some(port) => Ok(port.as_mut()),
            None => Err(StoreError::DatasetUnavailable),
        }
    }
}
struct Operation<T, F> {
    work: F,
    reply: oneshot::Sender<Credited<T>>,
    credit: OwnedSemaphorePermit,
}
impl<T: Send, F: FnOnce(&mut dyn OwnedStorage) -> Result<T, StoreError> + Send> Job
    for Operation<T, F>
{
    fn perform(self: Box<Self>, store: &mut dyn OwnedStorage) -> bool {
        let Self {
            work,
            reply,
            credit,
        } = *self;
        let result = work(store);
        store.refresh_access();
        let failed = result.is_err();
        let _ = reply.send(Credited { result, credit });
        failed
    }
}
enum Request {
    Run(Box<dyn Job>),
    Barrier(oneshot::Sender<StoreDrain>),
    Shutdown(oneshot::Sender<StoreDrain>),
}

pub fn spawn_store(
    store: impl ValueStore + 'static,
    limits: StoreWorkerLimits,
) -> Result<(StoreWorker, StoreWorkerTask), StoreError> {
    spawn_owner(store, None, limits)
}
/// Values and datasets share one mailbox, credit pool, thread and shutdown barrier.
pub fn spawn_storage(
    store: impl ValueStore + 'static,
    datasets: impl DatasetStorage + 'static,
    limits: StoreWorkerLimits,
) -> Result<(StoreWorker, StoreWorkerTask), StoreError> {
    spawn_owner(store, Some(Box::new(datasets)), limits)
}
fn spawn_owner(
    store: impl ValueStore + 'static,
    datasets: Option<Box<dyn DatasetStorage>>,
    limits: StoreWorkerLimits,
) -> Result<(StoreWorker, StoreWorkerTask), StoreError> {
    let confidential_values = store.supports_confidential();
    let confidential_datasets = datasets.as_ref().is_some_and(|s| s.supports_confidential());
    let dataset_read_charge = datasets
        .as_ref()
        .map(|port| {
            u32::try_from(port.read_charge()).map_err(|_| StoreError::Limit("dataset I/O charge"))
        })
        .transpose()?;
    let access = super::datasets::DatasetAccess {
        withdrawn: datasets
            .as_ref()
            .map(|port| port.read_access())
            .transpose()?
            .unwrap_or_default(),
        closed: false,
    };
    let (dataset_access_tx, dataset_access) = tokio::sync::watch::channel(access);
    let dataset_changes = datasets.as_ref().and_then(|port| port.changes());
    if limits.operations.get() > Semaphore::MAX_PERMITS
        || limits.bytes.get() as usize > Semaphore::MAX_PERMITS
    {
        return Err(StoreError::Limit("queue capacity"));
    }
    let (sender, receiver) = mpsc::channel(limits.operations.get());
    let budget = Arc::new(Semaphore::new(limits.bytes.get() as usize));
    let closed_budget = budget.clone();
    let (stopped_tx, stopped) = oneshot::channel();
    let thread = thread::Builder::new()
        .name("wes-storage".into())
        .spawn(move || {
            struct CloseBudget(Arc<Semaphore>);
            impl Drop for CloseBudget {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let _close = CloseBudget(closed_budget);
            storage_loop(
                Owner {
                    values: super::private::PolicyValues::new(store),
                    datasets,
                    pending_evictions: Default::default(),
                    evictions_more: false,
                    dataset_access: dataset_access_tx,
                },
                receiver,
            );
            let _ = stopped_tx.send(());
        })
        .map_err(|error| StoreError::backend("starting the storage worker", error))?;
    Ok((
        StoreWorker {
            sender,
            budget,
            limits,
            dataset_read_charge,
            confidential_values,
            confidential_datasets,
            dataset_access,
            dataset_changes,
        },
        StoreWorkerTask { thread, stopped },
    ))
}
impl StoreWorkerTask {
    /// Shutdown/drop all senders first. This joins physical exit and releases store ownership.
    /// Dropping this handle alone never aborts accepted operations.
    pub async fn join(self) -> Result<(), StoreError> {
        let _ = self.stopped.await;
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .map_err(|_| StoreError::Closed)?
            .map_err(|_| StoreError::Closed)
    }
}
impl StoreWorker {
    /// Mandatory disk sinks call this before entering acquisition. No producer is restarted.
    pub fn admit_dataset_policy(
        &self,
        policy: &wes_core::flow::FlowPolicy,
    ) -> Result<(), StoreError> {
        if policy.is_private()
            || policy.is_unknown()
            || (policy.is_confidential() && !self.confidential_datasets)
        {
            Err(StoreError::Restricted)
        } else {
            Ok(())
        }
    }

    pub(crate) fn dataset_changes(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<crate::storage::datasets::DatasetChanges>> {
        self.dataset_changes.clone()
    }
    /// Read a committed EventLog extension through the same owned admission as Dataset pages.
    /// A descriptor never creates a recording or producer; current read gates still apply.
    pub async fn eventlog_head(
        &self,
        reference: wes_core::DatasetRef,
        work: Option<super::datasets::ReadWork>,
    ) -> Result<DatasetInfo, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| owner.datasets()?.eventlog_head(&reference, work.as_ref()),
        )
        .await?
        .wait()
        .await
    }
    pub fn dataset_access(&self) -> tokio::sync::watch::Receiver<super::datasets::DatasetAccess> {
        self.dataset_access.clone()
    }
    /// Retain/size an existing handle as one serial operation. Acknowledged keep must establish
    /// the store's declared persistence even for an already retained handle. Never republishes it.
    pub async fn retain(&self, handle: ValueHandle) -> Result<StoredOutput, StoreError> {
        self.retain_as(handle, Retention::Protected).await
    }
    pub async fn retain_as(
        &self,
        handle: ValueHandle,
        reason: Retention,
    ) -> Result<StoredOutput, StoreError> {
        self.request(self.limits.bytes.get(), move |store| {
            let mut finish = || {
                if !store.keep_with_reason(&handle, reason)? {
                    return Err(StoreError::RetentionUnavailable);
                }
                let bytes = store.size(&handle)?.ok_or(StoreError::MissingValue)?;
                Ok(StoredOutput {
                    handle: handle.clone(),
                    bytes,
                    kept: true,
                    retention: store.retention(&handle)?,
                    retained_persistence: store.retained_persistence(),
                })
            };
            finish().map_err(|source| StoreError::Published {
                handle,
                source: Box::new(source),
            })
        })
        .await?
        .wait()
        .await
    }
    /// Inspect only a retained handle, then read/size it as one serial job. Never re-archive live
    /// leftovers during restart. Unknown response size reserves the entire worker payload budget.
    pub async fn recover(
        &self,
        handle: ValueHandle,
    ) -> Result<Option<super::RecoveredOutput>, StoreError> {
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            if !store.is_kept(&handle)? {
                return Ok(None);
            }
            let Some(loaded) = store.read(&handle)? else {
                return Ok(None);
            };
            if value_charge(&loaded.value, limit.into()).is_none() {
                return Err(StoreError::Limit("recovered value payload"));
            }
            let bytes = store.size(&handle)?.ok_or(StoreError::MissingValue)?;
            Ok(Some(super::RecoveredOutput {
                loaded,
                bytes,
                retention: store.retention(&handle)?,
            }))
        })
        .await?
        .wait()
        .await
    }
    /// Store, inspect size and (when selected) archive as one serial operation. A competing store
    /// cannot evict the new live value between these steps. Success is not a Result journal receipt.
    /// The owner joins this future; it never retries a possibly published handle on cancellation.
    pub async fn publish(
        &self,
        value: Value,
        policy: PublicationPolicy,
    ) -> Result<StoredOutput, StoreError> {
        if policy == PublicationPolicy::Protected {
            if !value.provenance().policy().allows_retention() {
                return Err(StoreError::Restricted);
            }
            if !value.data().is_storable_snapshot() {
                return Err(StoreError::NonMaterialized);
            }
        }
        let cost = value_charge(&value, self.limits.bytes.get().into())
            .ok_or(StoreError::Limit("value payload"))? as u32;
        let cost = if owner::has_datasets(&value)? {
            cost.max(
                self.dataset_read_charge
                    .ok_or(StoreError::DatasetUnavailable)?,
            )
        } else {
            cost
        };
        self.request(cost, move |store| {
            let handle = store.store(&value)?;
            let mut finish = || -> Result<StoredOutput, StoreError> {
                let bytes = store.size(&handle)?.ok_or(StoreError::MissingValue)?;
                let kept = match policy {
                    PublicationPolicy::Protected => {
                        if !store.keep_with_reason(&handle, Retention::Protected)? {
                            return Err(StoreError::RetentionUnavailable);
                        }
                        true
                    }
                    PublicationPolicy::AutomaticUpToBytes(limit)
                        if value.data().is_storable_snapshot()
                            && !value.provenance().policy().is_confidential()
                            && store.automatic_retention_allowed(&handle)?
                            && store
                                .retention_size(&handle)?
                                .is_some_and(|bytes| bytes <= limit) =>
                    {
                        if !store.keep_with_reason(&handle, Retention::Automatic)? {
                            return Err(StoreError::RetentionUnavailable);
                        }
                        true
                    }
                    _ => false,
                };
                Ok(StoredOutput {
                    handle: handle.clone(),
                    bytes,
                    kept,
                    retention: store.retention(&handle)?,
                    retained_persistence: if kept {
                        store.retained_persistence()
                    } else {
                        crate::history::Persistence::Volatile
                    },
                })
            };
            finish().map_err(|source| StoreError::Published {
                handle,
                source: Box::new(source),
            })
        })
        .await?
        .wait()
        .await
    }
    async fn request<T: Send + 'static>(
        &self,
        cost: u32,
        work: impl FnOnce(&mut dyn ValueStore) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<PendingStore<T>, StoreError> {
        self.request_owner(cost, move |owner| work(owner.values()))
            .await
    }
    async fn request_owner<T: Send + 'static>(
        &self,
        cost: u32,
        work: impl FnOnce(&mut dyn OwnedStorage) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<PendingStore<T>, StoreError> {
        if self.sender.is_closed() {
            return Err(StoreError::Closed);
        }
        if cost > self.limits.bytes.get() {
            return Err(StoreError::Limit("queue payload"));
        }
        let credit = self
            .budget
            .clone()
            .acquire_many_owned(cost)
            .await
            .map_err(|_| StoreError::Closed)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::Run(Box::new(Operation {
                work,
                reply,
                credit,
            })))
            .await
            .map_err(|_| StoreError::Closed)?;
        Ok(PendingStore(receive))
    }
    /// Joined blocking history jobs use the same credited FIFO owner as async callers.
    pub fn retained_dataset_publication(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::history::RetainedDatasetPublication>> {
        self.dataset_read_charge?;
        Some(std::sync::Arc::new(WorkspacePublication {
            worker: self.clone(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }
    async fn workspace_roots(
        &self,
        generation: String,
        handles: Option<Vec<ValueHandle>>,
    ) -> Result<Vec<ValueHandle>, StoreError> {
        if self.dataset_read_charge.is_none() {
            return Ok(vec![]);
        }
        self.request_owner(self.limits.bytes.get(), move |owner| {
            match handles {
                Some(handles) => {
                    // Declared retained history is not authority to resurrect absent/transient bytes.
                    let mut retained = Vec::new();
                    for handle in handles {
                        if owner.retained_value_exists(&handle)? {
                            retained.push(handle);
                        }
                    }
                    owner
                        .datasets_mut()?
                        .protect_workspace(&generation, &retained)?;
                    Ok(retained)
                }
                None => {
                    owner.datasets_mut()?.retire_workspace(&generation)?;
                    Ok(vec![])
                }
            }
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_inspect(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<DatasetInfo, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| owner.datasets()?.inspect(&reference),
        )
        .await?
        .wait()
        .await
    }
    pub async fn dataset_head(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<DatasetInfo, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| owner.datasets()?.head(&reference),
        )
        .await?
        .wait()
        .await
    }
    pub async fn dataset_snapshot(
        &self,
        reference: wes_core::DatasetRef,
        selection: super::datasets::DatasetSnapshot,
    ) -> Result<DatasetInfo, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| owner.datasets()?.snapshot(&reference, selection),
        )
        .await?
        .wait()
        .await
    }
    pub async fn dataset_retention_preview(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<super::datasets::DatasetRetentionCost, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| {
                let inventory = owner.datasets()?.retention_preview(&reference)?;
                if inventory.reference != reference {
                    return Err(StoreError::Conflict);
                }
                let mut total = inventory.object_bytes;
                let mut shared = inventory.shared_object_bytes;
                let mut captured = 0u64;
                for capture in inventory.captures {
                    let bytes = capture.value.bytes;
                    total = total
                        .checked_add(bytes)
                        .ok_or(StoreError::Limit("retention bytes"))?;
                    captured = captured
                        .checked_add(bytes)
                        .ok_or(StoreError::Limit("retention bytes"))?;
                    if capture.shared
                        || owner.retained_value_exists(&ValueHandle::new(&capture.value.handle)?)?
                    {
                        shared = shared
                            .checked_add(bytes)
                            .ok_or(StoreError::Limit("retention bytes"))?;
                    }
                }
                let exclusive = total
                    .checked_sub(shared)
                    .ok_or(StoreError::DatasetCorrupt)?;
                // Both inventories were read by the same FIFO owner; no mutation/reservation was made.
                Ok(super::datasets::DatasetRetentionCost {
                    reference,
                    total_bytes: total,
                    shared_bytes: shared,
                    exclusive_bytes: exclusive,
                    captured_source_bytes: captured,
                    catalog_revision: inventory.catalog_revision,
                })
            },
        )
        .await?
        .wait()
        .await
    }
    pub async fn dataset_plan_delete(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<super::datasets::DatasetDeletePlan, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            owner.datasets_mut()?.plan_delete(&reference)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_delete(
        &self,
        token: String,
        references: bool,
        protected: bool,
    ) -> Result<super::datasets::DatasetCleanup, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            let mut cleanup = owner
                .datasets_mut()?
                .delete(&token, references, protected)?;
            finish_dataset_cleanup(owner, &mut cleanup);
            Ok(cleanup)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_collect(&self) -> Result<super::datasets::DatasetCleanup, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            let mut cleanup = owner.datasets_mut()?.collect()?;
            finish_dataset_cleanup(owner, &mut cleanup);
            Ok(cleanup)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_withdraw(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<Vec<String>, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            owner.datasets_mut()?.withdraw(&reference)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_resume(
        &self,
        request: super::datasets::DatasetResume,
    ) -> Result<super::datasets::DatasetWriterAdmission, StoreError> {
        self.enqueue_dataset_resume(request).await?.wait().await
    }
    /// Dropping this acknowledgement releases any delivered process ownership;
    /// it cannot cancel the already admitted local commit.
    pub async fn enqueue_dataset_resume(
        &self,
        request: super::datasets::DatasetResume,
    ) -> Result<PendingStore<super::datasets::DatasetWriterAdmission>, StoreError> {
        let cost = self.dataset_payload_charge(Some(&request.checkpoint), &[], &[])?;
        self.request_owner(cost, move |owner| {
            owner.validate_checkpoint(&request.checkpoint, &request.policy, true, false)?;
            owner.datasets_mut()?.resume(request)
        })
        .await
    }
    pub async fn dataset_reconcile_store(&self) -> Result<crate::history::Persistence, StoreError> {
        self.request_owner(self.limits.bytes.get(), |owner| {
            owner.datasets_mut()?.reconcile_store()
        })
        .await?
        .wait()
        .await
    }
    /// The shared FIFO mailbox orders this recovery behind all admitted physical writes.
    pub async fn dataset_reconcile(
        &self,
        identity: super::datasets::DatasetWriteSelection,
    ) -> Result<super::datasets::DatasetReconciliation, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            owner.datasets_mut()?.reconcile_owned(&identity)
        })
        .await?
        .wait()
        .await
    }
    /// Joined read of a checkpoint and its exact protected source; it confers no execution grant.
    pub async fn dataset_continuation(
        &self,
        run: String,
    ) -> Result<super::datasets::DatasetContinuation, StoreError> {
        let limit = self.limits.bytes.get();
        self.request_owner(limit, move |owner| {
            let identity = super::datasets::DatasetWriteSelection {
                role: super::datasets::DatasetWriteRole::Analysis,
                run: run.clone(),
            };
            let receipt = owner.datasets_mut()?.reconcile_owned(&identity)?;
            if receipt.outcome == super::datasets::DatasetWriteOutcome::Unknown {
                return Err(StoreError::DatasetRecoveryUnknown);
            }
            owner.captured_analysis(&run, limit)
        })
        .await?
        .wait()
        .await
    }
    /// Bounded joined read, with no reconciliation, reservation or write admission.
    pub async fn dataset_analysis_read(
        &self,
        run: String,
    ) -> Result<super::datasets::DatasetContinuation, StoreError> {
        let limit = self.limits.bytes.get();
        self.request_owner(limit, move |owner| owner.captured_analysis(&run, limit))
            .await?
            .wait()
            .await
    }
    /// Read only the owned analysis's protected input. Range strings and ownership
    /// are selected by the workspace, not by a raw dataset or value-handle literal.
    pub async fn dataset_source_excerpt(
        &self,
        run: String,
        from: u64,
        rows_or_bytes: usize,
    ) -> Result<Value, StoreError> {
        let limit = self.limits.bytes.get();
        self.request_owner(limit, move |owner| {
            let captured = owner.captured_analysis(&run, limit)?;
            let value = crate::scan::read_source_excerpt(
                owner.datasets()?,
                &captured.source,
                &captured.checkpoint,
                from,
                rows_or_bytes,
            )?;
            owner.datasets()?.inspect(&captured.reference)?;
            if !owner.validate_read(
                &ValueHandle::new(&captured.checkpoint.source.handle)?,
                &captured.source,
            )? {
                return Err(StoreError::MissingValue);
            }
            Ok(value)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_create(
        &self,
        request: DatasetCreate,
    ) -> Result<super::datasets::DatasetWriterAdmission, StoreError> {
        self.enqueue_dataset_create(request).await?.wait().await
    }
    /// Prefix publication and writer ownership share one FIFO operation, even
    /// when the requester abandons the acknowledgement after admission.
    pub async fn enqueue_dataset_create(
        &self,
        request: DatasetCreate,
    ) -> Result<PendingStore<super::datasets::DatasetWriterAdmission>, StoreError> {
        let cost = self.dataset_payload_charge(request.checkpoint.as_ref(), &[], &[])?;
        self.request_owner(cost, move |owner| {
            if let Some(checkpoint) = &request.checkpoint {
                owner.validate_checkpoint(checkpoint, &request.policy, true, true)?;
            }
            owner.datasets_mut()?.create(request)
        })
        .await
    }
    /// Admission is irrevocable: dropping the receipt does not cancel a physical commit.
    pub async fn enqueue_dataset_append(
        &self,
        request: DatasetAppend,
    ) -> Result<PendingStore<wes_core::DatasetRef>, StoreError> {
        let cost = self.dataset_payload_charge(
            request.checkpoint.as_ref(),
            &request.rows,
            &request.coverage,
        )?;
        self.request_owner(cost, move |owner| {
            if let Some(checkpoint) = &request.checkpoint {
                owner.validate_checkpoint(checkpoint, &request.policy, false, false)?;
            }
            owner.datasets_mut()?.append(request)
        })
        .await
    }
    fn dataset_payload_charge(
        &self,
        checkpoint: Option<&super::datasets::AnalysisCheckpoint>,
        rows: &[super::datasets::DatasetRow],
        coverage: &[wes_core::framing::Rejection],
    ) -> Result<u32, StoreError> {
        let limit = self.limits.bytes.get() as u64;
        let mut cost = self
            .dataset_read_charge
            .ok_or(StoreError::DatasetUnavailable)? as u64;
        for row in rows {
            cost = cost
                .checked_add(
                    value_charge(&row.value, limit).ok_or(StoreError::Limit("dataset payload"))?,
                )
                .ok_or(StoreError::Limit("dataset payload"))?;
        }
        for row in coverage {
            let bytes = super::datasets::rejection_charge(row)
                .ok_or(StoreError::Limit("coverage payload"))?;
            cost = cost
                .checked_add(bytes)
                .ok_or(StoreError::Limit("coverage payload"))?;
        }
        if let Some(checkpoint) = checkpoint {
            for value in [&checkpoint.state, &checkpoint.context] {
                cost = cost
                    .checked_add(
                        value_charge(value, limit)
                            .ok_or(StoreError::Limit("checkpoint payload"))?,
                    )
                    .ok_or(StoreError::Limit("checkpoint payload"))?;
            }
            for bytes in [
                checkpoint.state_schema.encoded(),
                checkpoint.context_schema.encoded(),
                checkpoint.item_schema.encoded(),
                checkpoint.captured_program.as_bytes(),
                checkpoint.decoder_carry.as_slice(),
            ] {
                cost = cost
                    .checked_add(
                        (bytes.len() as u64)
                            .checked_mul(8)
                            .ok_or(StoreError::Limit("checkpoint payload"))?,
                    )
                    .ok_or(StoreError::Limit("checkpoint payload"))?;
            }
            // Raw source validation belongs to this job, including its retained encoding.
            cost = cost
                .checked_add(
                    checkpoint
                        .source
                        .bytes
                        .checked_mul(4)
                        .ok_or(StoreError::Limit("checkpoint source"))?,
                )
                .ok_or(StoreError::Limit("checkpoint source"))?;
        }
        u32::try_from(cost)
            .ok()
            .filter(|n| *n <= self.limits.bytes.get())
            .ok_or(StoreError::Limit("dataset payload"))
    }
    pub async fn capture_scan_source(
        &self,
        value: Value,
    ) -> Result<super::datasets::CapturedValue, StoreError> {
        let limit = self.limits.bytes.get();
        if !value.data().is_storable_snapshot()
            || value.shape().contains_meta()
            || value.provenance().policy().is_private()
            || value.provenance().policy().is_unknown()
        {
            return Err(StoreError::Restricted);
        }
        if !value.provenance().policy().allows_retention()
            || (value.provenance().policy().is_confidential() && !self.confidential_values)
        {
            return Err(StoreError::Restricted);
        }
        value_charge(&value, limit as u64).ok_or(StoreError::Limit("captured source"))?;
        self.request(limit, move |store| {
            use sha2::{Digest, Sha256};
            let handle = store.store(&value)?;
            let finish = |store: &mut dyn ValueStore| {
                if !store.keep_with_reason(&handle, Retention::Protected)? {
                    return Err(StoreError::RetentionUnavailable);
                }
                let bytes = store.encoded(&handle)?.ok_or(StoreError::MissingValue)?;
                Ok(super::datasets::CapturedValue {
                    handle: handle.to_string(),
                    digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
                    bytes: bytes.len() as u64,
                })
            };
            finish(store).map_err(|source| StoreError::Published {
                handle,
                source: Box::new(source),
            })
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_checkpoint(
        &self,
        reference: wes_core::DatasetRef,
    ) -> Result<Option<super::datasets::AnalysisCheckpoint>, StoreError> {
        self.request_owner(self.limits.bytes.get(), move |owner| {
            let info = owner.datasets()?.inspect(&reference)?;
            let checkpoint = owner.datasets()?.checkpoint(&reference)?;
            if let Some(saved) = &checkpoint {
                owner.validate_checkpoint(saved, &info.policy, true, false)?;
            }
            Ok(checkpoint)
        })
        .await?
        .wait()
        .await
    }
    pub async fn dataset_page(
        &self,
        reference: wes_core::DatasetRef,
        request: PageRequest,
    ) -> Result<DatasetPage, StoreError> {
        self.dataset_stream_page(reference, wes_core::DatasetStream::Outputs, request)
            .await
    }
    pub async fn dataset_stream_page(
        &self,
        reference: wes_core::DatasetRef,
        stream: wes_core::DatasetStream,
        request: PageRequest,
    ) -> Result<DatasetPage, StoreError> {
        self.request_owner(
            self.dataset_read_charge
                .ok_or(StoreError::DatasetUnavailable)?,
            move |owner| match stream {
                wes_core::DatasetStream::Outputs => owner.datasets()?.page(&reference, request),
                wes_core::DatasetStream::Coverage => {
                    owner.datasets()?.coverage_page(&reference, request)
                }
            },
        )
        .await?
        .wait()
        .await
    }
    pub async fn dataset_coverage_page(
        &self,
        reference: wes_core::DatasetRef,
        request: PageRequest,
    ) -> Result<DatasetPage, StoreError> {
        self.dataset_stream_page(reference, wes_core::DatasetStream::Coverage, request)
            .await
    }
    /// Acknowledges admission, not completion or durability. Calculation is bounded to one million
    /// shape/data nodes and depth 256; decimal charges never expand scientific notation.
    pub async fn enqueue_store(
        &self,
        value: Value,
    ) -> Result<PendingStore<ValueHandle>, StoreError> {
        let cost = value_charge(&value, self.limits.bytes.get().into())
            .ok_or(StoreError::Limit("value payload"))? as u32;
        let cost = if owner::has_datasets(&value)? {
            cost.max(
                self.dataset_read_charge
                    .ok_or(StoreError::DatasetUnavailable)?,
            )
        } else {
            cost
        };
        self.request(cost, move |store| store.store(&value)).await
    }
    pub async fn store(&self, value: Value) -> Result<ValueHandle, StoreError> {
        self.enqueue_store(value).await?.wait().await
    }
    /// Unknown response size reserves the full byte allowance until its receipt is consumed. The
    /// leaf codec/store must also bound its own I/O/allocation; this checks the returned value too.
    pub async fn read(&self, handle: ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            let result = store.read(&handle)?;
            if let Some(loaded) = &result {
                value_charge(&loaded.value, limit.into())
                    .ok_or(StoreError::Limit("read response"))?;
            }
            Ok(result)
        })
        .await?
        .wait()
        .await
    }
    /// Revalidate current owned access after encoding without decoding an immutable handle again.
    pub async fn validate_read(
        &self,
        handle: ValueHandle,
        captured: Value,
    ) -> Result<bool, StoreError> {
        let charge = self.dataset_read_charge.unwrap_or(0).max(
            value_charge(&captured, self.limits.bytes.get().into())
                .ok_or(StoreError::Limit("read authorization"))?
                .try_into()
                .map_err(|_| StoreError::Limit("read authorization"))?,
        );
        self.request_owner(charge, move |owner| owner.validate_read(&handle, &captured))
            .await?
            .wait()
            .await
    }
    /// Compare the complete captured value on the storage owner. Retained codecs can decode
    /// the same immutable result into new allocations; pointer identity is not a read revision.
    pub async fn read_matches(
        &self,
        handle: ValueHandle,
        captured: Value,
    ) -> Result<bool, StoreError> {
        let limit = self.limits.bytes.get();
        let captured_charge =
            value_charge(&captured, limit.into()).ok_or(StoreError::Limit("read revalidation"))?;
        self.request(limit, move |store| {
            let Some(loaded) = store.read(&handle)? else {
                return Ok(false);
            };
            value_charge(
                &loaded.value,
                u64::from(limit).saturating_sub(captured_charge),
            )
            .ok_or(StoreError::Limit("read revalidation"))?;
            Ok(loaded.value.same_snapshot(&captured) || loaded.value == captured)
        })
        .await?
        .wait()
        .await
    }
    pub async fn encoded(&self, handle: ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            let result = store.encoded(&handle)?;
            if result
                .as_ref()
                .is_some_and(|bytes| bytes.len().saturating_add(256) > limit as usize)
            {
                return Err(StoreError::Limit("encoded response"));
            }
            Ok(result)
        })
        .await?
        .wait()
        .await
    }
    pub async fn size(&self, handle: ValueHandle) -> Result<Option<u64>, StoreError> {
        self.request(256, move |store| store.size(&handle))
            .await?
            .wait()
            .await
    }
    pub async fn release(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(self.limits.bytes.get(), move |store| store.release(&handle))
            .await?
            .wait()
            .await
    }
    /// Reclaim a superseded transient output only if it is still unkept. The retained check and
    /// release share one serial store job, so a previously admitted keep cannot race this cleanup.
    pub async fn release_unkept(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(self.limits.bytes.get(), move |store| {
            if store.is_kept(&handle)? {
                Ok(false)
            } else {
                store.release(&handle)
            }
        })
        .await?
        .wait()
        .await
    }
    pub async fn keep(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(self.limits.bytes.get(), move |store| {
            store.keep_with_reason(&handle, Retention::Protected)
        })
        .await?
        .wait()
        .await
    }
    pub async fn is_kept(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(self.limits.bytes.get(), move |store| store.is_kept(&handle))
            .await?
            .wait()
            .await
    }
    pub async fn retention(&self, handle: ValueHandle) -> Result<Retention, StoreError> {
        self.request(256, move |store| store.retention(&handle))
            .await?
            .wait()
            .await
    }
    pub async fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        self.request(1024, move |store| store.retention_usage())
            .await?
            .wait()
            .await
    }
    pub async fn take_evicted(&self, maximum: NonZeroUsize) -> Result<EvictionBatch, StoreError> {
        let cost = maximum
            .get()
            .checked_mul(128)
            .and_then(|bytes| bytes.checked_add(256))
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or(StoreError::Limit("eviction response"))?;
        self.request(cost, move |store| store.take_evicted(maximum.get()))
            .await?
            .wait()
            .await
    }
    /// Completion counts are not durability claims. The concrete store defines its sync boundary.
    pub async fn flush(&self) -> Result<StoreDrain, StoreError> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::Barrier(reply))
            .await
            .map_err(|_| StoreError::Closed)?;
        receive.await.map_err(|_| StoreError::Closed)
    }
    /// Closes admission and drains accepted work, including requests queued behind this message.
    /// Acknowledgement follows store destruction and release of its directory lock.
    pub async fn shutdown(&self) -> Result<StoreDrain, StoreError> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::Shutdown(reply))
            .await
            .map_err(|_| StoreError::Closed)?;
        receive.await.map_err(|_| StoreError::Closed)
    }
}
fn storage_loop(mut store: impl OwnedStorage, mut receiver: mpsc::Receiver<Request>) {
    let mut report = StoreDrain::default();
    let mut shutdown = vec![];
    while let Some(request) = receiver.blocking_recv() {
        match request {
            Request::Run(job) => {
                report.attempted = report.attempted.saturating_add(1);
                report.failed = report
                    .failed
                    .saturating_add(u64::from(job.perform(&mut store)));
            }
            Request::Barrier(reply) => {
                let _ = reply.send(report);
            }
            Request::Shutdown(reply) => {
                receiver.close();
                shutdown.push(reply);
            }
        }
    }
    drop(store);
    for reply in shutdown {
        let _ = reply.send(report);
    }
}

/// Physical value release precedes clearing the durable cleanup inventory.
/// Failure leaves an acknowledged deletion with explicit pending cleanup; it
/// never replays the deletion or discards a shared active capture.
fn finish_dataset_cleanup(
    owner: &mut dyn OwnedStorage,
    cleanup: &mut super::datasets::DatasetCleanup,
) {
    let mut released = Vec::new();
    for capture in &cleanup.released_captures {
        let result = (|| {
            let handle = ValueHandle::new(&capture.handle)?;
            if owner.datasets()?.protects_value(&handle)? {
                return Ok(true);
            }
            owner.values().release(&handle)?;
            Ok::<_, StoreError>(true)
        })();
        match result {
            Ok(true) => released.push(capture.clone()),
            _ => cleanup.complete = false,
        }
    }
    if !released.is_empty()
        && owner
            .datasets_mut()
            .and_then(|p| p.acknowledge_cleanup(&released))
            .is_err()
    {
        cleanup.complete = false;
    }
}
