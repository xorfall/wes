//! Serial storage ownership off the runtime loop. Receipt cancellation does not retract admitted I/O.
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
}
pub struct StoreWorkerTask {
    thread: thread::JoinHandle<()>,
    stopped: oneshot::Receiver<()>,
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
    fn perform(self: Box<Self>, store: &mut dyn ValueStore) -> bool;
}
struct Operation<T, F> {
    work: F,
    reply: oneshot::Sender<Credited<T>>,
    credit: OwnedSemaphorePermit,
}
impl<T: Send, F: FnOnce(&mut dyn ValueStore) -> Result<T, StoreError> + Send> Job
    for Operation<T, F>
{
    fn perform(self: Box<Self>, store: &mut dyn ValueStore) -> bool {
        let Self {
            work,
            reply,
            credit,
        } = *self;
        let result = work(store);
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
        .name("wes-values".into())
        .spawn(move || {
            struct CloseBudget(Arc<Semaphore>);
            impl Drop for CloseBudget {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let _close = CloseBudget(closed_budget);
            storage_loop(store, receiver);
            let _ = stopped_tx.send(());
        })
        .map_err(|error| StoreError::backend("starting the storage worker", error))?;
    Ok((
        StoreWorker {
            sender,
            budget,
            limits,
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
        self.request(1024, move |store| {
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
            if value.provenance().policy().is_private() {
                return Err(StoreError::Restricted);
            }
            if !value.data().is_materialized() {
                return Err(StoreError::NonMaterialized);
            }
        }
        let cost = value_charge(&value, self.limits.bytes.get().into())
            .ok_or(StoreError::Limit("value payload"))? as u32;
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
                        if bytes <= limit
                            && value.data().is_materialized()
                            && !value.provenance().policy().is_private() =>
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
    /// Acknowledges admission, not completion or durability. Calculation is bounded to one million
    /// shape/data nodes and depth 256; decimal charges never expand scientific notation.
    pub async fn enqueue_store(
        &self,
        value: Value,
    ) -> Result<PendingStore<ValueHandle>, StoreError> {
        let cost = value_charge(&value, self.limits.bytes.get().into())
            .ok_or(StoreError::Limit("value payload"))? as u32;
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
        self.request(256, move |store| store.release(&handle))
            .await?
            .wait()
            .await
    }
    /// Reclaim a superseded transient output only if it is still unkept. The retained check and
    /// release share one serial store job, so a previously admitted keep cannot race this cleanup.
    pub async fn release_unkept(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(256, move |store| {
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
        self.request(256, move |store| {
            store.keep_with_reason(&handle, Retention::Protected)
        })
        .await?
        .wait()
        .await
    }
    pub async fn is_kept(&self, handle: ValueHandle) -> Result<bool, StoreError> {
        self.request(256, move |store| store.is_kept(&handle))
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
fn storage_loop(store: impl ValueStore, mut receiver: mpsc::Receiver<Request>) {
    let mut store = super::private::PolicyValues::new(store);
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
