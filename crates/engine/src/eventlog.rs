//! A typed archive owns its write lifetime, never the producer's cancellation.
use crate::{
    storage::{StoreError, StoreWorker, datasets::*},
    streams::{StreamError, archive},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use wes_core::{DatasetRef, contracts::ResolvedContractBundle, flow::FlowPolicy};
pub(crate) mod intent;
pub(crate) mod owner;
mod validation;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub records: u64,
    pub bytes: u64,
    pub work: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            records: wes_budgets::get("dataset.writer.records"),
            bytes: wes_budgets::get("dataset.writer.bytes"),
            work: wes_budgets::get("dataset.writer.work"),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    Recording,
    Draining,
    Stopped,
    Incomplete,
    Unconfirmed,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub phase: Phase,
    pub reference: DatasetRef,
    pub coverage: EventLogCoverage,
    pub charged_bytes: u64,
    pub charged_work: u64,
}
#[derive(Clone)]
pub struct Handle {
    branch: Arc<archive::Branch>,
    status: watch::Receiver<Status>,
}
impl Handle {
    /// Stop accepting at an exact branch boundary. The caller waits for the terminal status;
    /// this request alone is neither a disk receipt nor a source cancellation.
    pub fn stop(&self) {
        self.branch.close(archive::End::Manual);
    }
    pub fn snapshot(&self) -> Status {
        self.status.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
}
/// Own and join the writer independently of the source. Dropping requests a
/// bounded stop; it cannot acknowledge disk completion or cancel the producer.
#[must_use = "retain and join the recording lifetime"]
pub struct Task {
    branch: Arc<archive::Branch>,
    status: watch::Sender<Status>,
    task: Option<tokio::task::JoinHandle<Status>>,
}
impl Task {
    pub async fn join(mut self) -> Result<Status, tokio::task::JoinError> {
        let result = self.task.take().expect("owned recording task").await;
        if result.is_err() {
            let mut failed = self.status.borrow().clone();
            failed.phase = Phase::Unconfirmed;
            failed.coverage.pending = None;
            self.status.send_replace(failed);
        }
        result
    }
    pub async fn stop(self) -> Result<Status, tokio::task::JoinError> {
        self.branch.close(archive::End::Manual);
        self.join().await
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.branch.close(archive::End::Manual);
    }
}
/// Preparation has no detached task. It must be attached to the exact admitted source run
/// and its `run` future retained/joined by a lifetime owner.
#[must_use]
pub struct Prepared {
    pub(crate) receiver: archive::Receiver,
    worker: StoreWorker,
    schema: ResolvedContractBundle,
    policy: FlowPolicy,
    status: watch::Sender<Status>,
    current: Status,
    limits: Limits,
    source: SourceExtent,
    owner: DatasetWriteOwner,
    _lease: DatasetWriteLease,
}
impl Prepared {
    /// Join an empty local preparation after producer admission was refused.
    /// This records a source-failed boundary without invoking or retrying that source.
    pub(crate) fn reject_source(&self) -> Result<(), StoreError> {
        if self.receiver.status.borrow().first.is_none() {
            self.receiver
                .branch
                .attach(self.current.coverage.first)
                .map_err(|_| StoreError::Conflict)?;
        }
        self.receiver.branch.close(archive::End::SourceFailed);
        Ok(())
    }
    /// Start only after this branch was attached to its exact source boundary.
    /// The returned lifetime must be retained and joined, including after Stop.
    pub fn start(self) -> Result<Task, StoreError> {
        if self.receiver.status.borrow().first != Some(self.current.coverage.first) {
            return Err(StoreError::Conflict);
        }
        Ok(Task {
            branch: self.receiver.branch.clone(),
            status: self.status.clone(),
            task: Some(tokio::spawn(self.run())),
        })
    }
    pub async fn prepare(
        worker: StoreWorker,
        run: String,
        owner_run: String,
        schema: ResolvedContractBundle,
        policy: FlowPolicy,
        limits: Limits,
        first: u64,
    ) -> Result<(Self, Handle), StoreError> {
        let receiver =
            archive::Branch::prepare().map_err(|_| StoreError::Limit("recording ingress"))?;
        Self::prepare_receiver(
            worker, run, owner_run, schema, policy, limits, first, receiver,
        )
        .await
    }
    /// Attach first, then prepare storage while the bounded branch holds newly accepted events.
    /// The reported start is the actual admission boundary, never the beginning of a display window.
    pub async fn from_next(
        worker: StoreWorker,
        source: &crate::streams::StreamHandle,
        owner_run: String,
        schema: ResolvedContractBundle,
        policy: FlowPolicy,
        limits: Limits,
    ) -> Result<(Self, Handle), StoreError> {
        let receiver =
            archive::Branch::prepare().map_err(|_| StoreError::Limit("recording ingress"))?;
        source
            .attach_archive(receiver.branch.clone())
            .map_err(|_| StoreError::Conflict)?;
        let first = receiver.status.borrow().first.ok_or(StoreError::Conflict)?;
        Self::prepare_receiver(
            worker,
            source.snapshot().run.id().to_string(),
            owner_run,
            schema,
            policy,
            limits,
            first,
            receiver,
        )
        .await
    }
    async fn prepare_receiver(
        worker: StoreWorker,
        run: String,
        owner_run: String,
        schema: ResolvedContractBundle,
        policy: FlowPolicy,
        limits: Limits,
        first: u64,
        receiver: archive::Receiver,
    ) -> Result<(Self, Handle), StoreError> {
        worker.admit_dataset_policy(&policy)?;
        if first == 0
            || limits.records == 0
            || limits.records > wes_budgets::get("dataset.writer.records")
            || limits.work == 0
            || limits.work > wes_budgets::get("dataset.writer.work")
            || limits.bytes == 0
            || limits.bytes > wes_budgets::get("dataset.writer.bytes")
            || policy.is_private()
            || policy.is_unknown()
            || crate::storage::ValueHandle::new(&run).is_err()
            || crate::storage::ValueHandle::new(&owner_run).is_err()
        {
            return Err(StoreError::Restricted);
        }
        let owner = DatasetWriteOwner {
            role: DatasetWriteRole::Recording,
            lineage: owner_run.clone(),
            run: owner_run,
        };
        let epoch = uuid::Uuid::new_v4().to_string();
        let coverage = EventLogCoverage {
            run: run.clone(),
            epoch: epoch.clone(),
            first,
            accepted_through: first - 1,
            committed_through: first - 1,
            pending: Some(0),
            rejected: 0,
            termination: None,
        };
        let source = SourceExtent {
            identity: run,
            unit: SourceUnit::Records,
            start: first - 1,
            end: first - 1,
        };
        let admission = worker
            .dataset_create(DatasetCreate {
                coverage: None,
                owner: Some(owner.clone()),
                dataset: epoch,
                transaction: uuid::Uuid::new_v4().to_string(),
                kind: DatasetKind::EventLog,
                schema: schema.clone(),
                source: source.clone(),
                policy: policy.clone(),
                checkpoint: None,
                recording: Some(coverage.clone()),
            })
            .await?;
        let current = Status {
            phase: Phase::Prepared,
            reference: admission.reference,
            coverage,
            charged_bytes: 0,
            charged_work: 0,
        };
        let (status, updates) = watch::channel(current.clone());
        let handle = Handle {
            branch: receiver.branch.clone(),
            status: updates,
        };
        Ok((
            Self {
                receiver,
                worker,
                schema,
                policy,
                status,
                current,
                limits,
                source,
                owner,
                _lease: admission.lease,
            },
            handle,
        ))
    }
    pub(crate) fn branch(&self) -> Arc<archive::Branch> {
        self.receiver.branch.clone()
    }
    pub(crate) fn matches_run(&self, run: &crate::runtime::Run) -> bool {
        self.current.coverage.run == run.id().to_string() && self.current.coverage.first == 1
    }
    fn publish(&self) {
        self.status.send_replace(self.current.clone());
    }
    fn coverage(&self) -> EventLogCoverage {
        let mut coverage = self.current.coverage.clone();
        let observed = self.receiver.status.borrow().clone();
        coverage.accepted_through = observed.accepted_through.max(coverage.committed_through);
        coverage.pending = Some(coverage.accepted_through - coverage.committed_through);
        coverage.rejected = u64::from(observed.end == Some(archive::End::Rejected));
        coverage
    }
    async fn append(&mut self, request: DatasetAppend) -> Result<bool, StoreError> {
        let deadline = Duration::from_millis(wes_budgets::get("dataset.writer.wait.ms"));
        let commit = async {
            self.worker
                .enqueue_dataset_append(request)
                .await?
                .wait()
                .await
        };
        tokio::pin!(commit);
        let (result, timely) = tokio::select! {
            result = &mut commit => (result, true),
            _ = tokio::time::sleep(deadline) => {
                // Detach immediately, then join the admitted filesystem work. No cancellation
                // can erase an unknown commit or release its ingress credit prematurely.
                self.receiver.branch.close(archive::End::Overloaded);
                (commit.await, false)
            }
        };
        self.current.reference = result?;
        Ok(timely)
    }
    /// Commit one bounded batch at a time. Even a single event is flushed immediately.
    async fn run(mut self) -> Status {
        self.current.phase = Phase::Recording;
        self.publish();
        let mut failure = None;
        let mut restricted = false;
        while let Some(first) = self.receiver.recv().await {
            // Never wait to fill a batch. Ready items share one atomic commit, while a lone
            // event is flushed immediately. The entire batch retains original ingress credit.
            let items = self.receiver.take_ready(first);
            let schema = self.schema.clone();
            let current = self.current.clone();
            let policy = self.policy.clone();
            let limits = self.limits;
            let validated = tokio::task::spawn_blocking(move || {
                validation::batch(items, schema, current, policy, limits)
            })
            .await;
            let batch = match validated {
                Ok(batch) => batch,
                Err(_) => {
                    failure = Some(RecordingEnd::Unconfirmed);
                    self.receiver.branch.close(archive::End::Rejected);
                    break;
                }
            };
            let validation::Batch {
                items,
                rows,
                policy,
                charged_bytes,
                charged_work,
                failure: rejected,
                restricted: private,
            } = batch;
            self.current.charged_work = charged_work;
            failure = rejected;
            restricted = private;
            if failure.is_some() {
                self.receiver.branch.close(archive::End::Rejected);
            }
            if restricted || rows.is_empty() {
                drop(items);
                break;
            }
            let through = rows.last().unwrap().source_end;
            let mut coverage = self.coverage();
            coverage.committed_through = through;
            coverage.pending = Some(coverage.accepted_through.saturating_sub(through));
            let mut source = self.source.clone();
            source.end = through;
            let request = DatasetAppend {
                coverage: Vec::new(),
                owner: Some(self.owner.clone()),
                previous: self.current.reference.clone(),
                transaction: uuid::Uuid::new_v4().to_string(),
                rows,
                source: source.clone(),
                lifecycle: DatasetLifecycle::Open,
                policy: policy.clone(),
                checkpoint: None,
                recording: Some(coverage.clone()),
            };
            let result = self.append(request).await;
            drop(items); // joined before releasing any original producer credit
            match result {
                Ok(timely) => {
                    self.source = source;
                    self.policy = policy;
                    self.current.coverage = coverage;
                    self.current.charged_bytes = charged_bytes;
                    self.publish();
                    if !timely {
                        failure = Some(RecordingEnd::Overloaded);
                    }
                    if failure.is_some() {
                        break;
                    }
                }
                Err(
                    StoreError::DatasetUnconfirmed { .. }
                    | StoreError::DatasetAdmissionUnconfirmed { .. },
                ) => {
                    failure = Some(RecordingEnd::Unconfirmed);
                    break;
                }
                Err(_) => {
                    failure = Some(RecordingEnd::WriteFailed);
                    break;
                }
            }
        }
        self.current.phase = Phase::Draining;
        self.receiver.branch.close(archive::End::Manual);
        let observed = self.receiver.status.borrow().clone();
        let end = failure.unwrap_or(match observed.end {
            Some(archive::End::Natural) => RecordingEnd::Natural,
            Some(archive::End::Cancelled) => RecordingEnd::Cancelled,
            Some(archive::End::SourceFailed) => RecordingEnd::SourceFailed,
            Some(archive::End::Rejected) => RecordingEnd::Rejected,
            Some(archive::End::Overloaded) => RecordingEnd::Overloaded,
            _ => RecordingEnd::Manual,
        });
        self.receiver.discard_pending();
        if restricted {
            // No content-bearing successor may be written after private escalation.
            // The independent coarse control path also withdraws every derived read.
            self.current.phase = match self
                .worker
                .dataset_withdraw(self.current.reference.clone())
                .await
            {
                Ok(_) => Phase::Incomplete,
                Err(_) => Phase::Unconfirmed,
            };
            self.current.coverage.pending = None;
            self.publish();
            return self.current;
        }
        let mut coverage = self.coverage();
        coverage.termination = Some(end);
        self.current.coverage = coverage.clone();
        self.publish();
        let lifecycle = match end {
            RecordingEnd::Natural | RecordingEnd::Manual => DatasetLifecycle::Sealed,
            RecordingEnd::Cancelled => DatasetLifecycle::Cancelled,
            _ => DatasetLifecycle::Incomplete,
        };
        if end != RecordingEnd::Unconfirmed {
            let request = DatasetAppend {
                coverage: Vec::new(),
                owner: Some(self.owner.clone()),
                previous: self.current.reference.clone(),
                transaction: uuid::Uuid::new_v4().to_string(),
                rows: vec![],
                source: self.source.clone(),
                lifecycle,
                policy: self.policy.clone(),
                checkpoint: None,
                recording: Some(coverage),
            };
            match self.append(request).await {
                Ok(_) => {
                    self.current.phase =
                        if matches!(end, RecordingEnd::Natural | RecordingEnd::Manual) {
                            Phase::Stopped
                        } else {
                            Phase::Incomplete
                        }
                }
                Err(
                    StoreError::DatasetUnconfirmed { .. }
                    | StoreError::DatasetAdmissionUnconfirmed { .. },
                ) => self.current.phase = Phase::Unconfirmed,
                Err(_) => self.current.phase = Phase::Incomplete,
            }
        } else {
            self.current.phase = Phase::Unconfirmed;
            self.current.coverage.pending = None;
        }
        self.publish();
        self.current
    }
}

/// Starts exactly the supplied admitted producer with its archive already installed. The
/// returned source task joins both physical lifetimes; recording failure does not cancel it.
pub fn spawn(
    call: crate::providers::Call,
    invoker: Arc<dyn crate::streams::StreamingInvoker>,
    attribution: wes_core::Provenance,
    limits: crate::streams::Limits,
    parent: crate::driver::CancellationToken,
    prepared: Prepared,
) -> Result<(crate::streams::StreamHandle, crate::streams::StreamTask), StreamError> {
    if !prepared.matches_run(&call.run) {
        return Err(StreamError::Invalid);
    }
    let branch = prepared.branch();
    let (handle, source) = crate::streams::spawn_with_archives(
        call,
        invoker,
        attribution,
        limits,
        parent,
        None,
        None,
        vec![branch],
    )?;
    let cancellation = handle.cancellation_token();
    let archive = prepared
        .start()
        .expect("source installed its recording branch");
    let task = tokio::spawn(async move {
        let source_result = source.join().await;
        // Always join both physical lifetimes before reporting a failed lifetime owner.
        let archive_result = archive.join().await;
        assert!(
            source_result.is_ok() && archive_result.is_ok(),
            "Stream lifetime owner terminated unexpectedly"
        );
    });
    Ok((
        handle,
        crate::streams::StreamTask::joined(cancellation, task),
    ))
}
