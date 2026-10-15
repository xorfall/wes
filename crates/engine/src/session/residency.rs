//! Decoded cache maintenance; it never changes acknowledged execution state.
use super::*;
use crate::{
    graph::OutputRef,
    runtime::StoredOutputRef,
    storage::{LoadedValue, StoreError, ValueLease},
};
pub(super) struct ReadCompletion {
    read_epoch: String,
    output: OutputRef,
    captured: StoredOutputRef,
    member: String,
    cooperative: bool,
    result: Result<(LoadedValue, ValueLease), StoreError>,
    reply: oneshot::Sender<Result<sandbox::Observed, SessionError>>,
}
pub(super) async fn prepare_inputs(
    storage: &crate::storage::StoreWorker,
    ticket: &mut crate::runtime::RunTicket<BoundTask>,
) -> Result<(), wes_core::ErrorValue> {
    let read_error = |error: StoreError| error.read_error("Stored input preparation refused");
    let snapshots = ticket
        .deferred_inputs
        .values()
        .map(|captured| captured.snapshot.clone())
        .collect();
    ticket.input_holds.extend(
        storage
            .hold_snapshots(snapshots)
            .await
            .map_err(read_error)?,
    );
    if let BoundTask::Calculation(calc) = &mut ticket.payload {
        for captured in ticket.deferred_inputs.values() {
            calc.capture_source_provenance(&captured.snapshot.provenance);
        }
    }
    for (source, captured) in &ticket.deferred_inputs {
        if ticket.payload.stored_source_for(source) {
            let hold = storage
                .hold(captured.snapshot.handle.clone())
                .await
                .map_err(read_error)?
                .ok_or_else(|| read_error(StoreError::MissingValue))?;
            ticket.input_holds.push(hold);
            continue;
        }
        if let Some(paths) = ticket.payload.stored_field_paths(source) {
            let mut fields = std::collections::BTreeMap::new();
            for path in &paths {
                let mut selection = captured.snapshot.selection.clone();
                selection.extend_from_slice(path);
                let snapshot = storage
                    .describe(captured.snapshot.handle.clone(), selection)
                    .await
                    .map_err(read_error)?
                    .ok_or_else(|| read_error(StoreError::MissingValue))?;
                if snapshot.digest != captured.snapshot.digest {
                    return Err(read_error(StoreError::Conflict));
                }
                let (loaded, hold) = storage.materialize(snapshot).await.map_err(read_error)?;
                fields.insert(path.clone(), loaded.value);
                ticket.input_holds.push(hold);
            }
            ticket.payload.prepare_fields(source, &fields);
            ticket.prepared_fields.insert(source.clone(), paths);
        } else {
            let (loaded, hold) = storage
                .materialize(captured.snapshot.clone())
                .await
                .map_err(read_error)?;
            ticket.inputs.insert(source.clone(), loaded.value);
            ticket.input_holds.push(hold);
        }
    }
    Ok(())
}
impl Actor {
    pub(super) fn prepare_read(
        &mut self,
        member: Option<&str>,
        cooperative: bool,
        reply: &mut Option<oneshot::Sender<Result<sandbox::Observed, SessionError>>>,
    ) -> bool {
        let Some(member) = member else {
            return false;
        };
        let mut parts = member.split('.');
        let Some(output) = parts.next().and_then(|name| self.workspace.resolve(name)) else {
            return false;
        };
        let crate::runtime::OutputState::Stored(captured) =
            self.workspace.runtime().output(&output)
        else {
            return false;
        };
        let fields = parts.map(str::to_owned).collect::<Vec<_>>();
        let refuse = if self.workspace.runtime().is_closed() {
            Some(SessionError::Stopped)
        } else if captured.snapshot.provenance.policy().is_private()
            || captured.snapshot.provenance.policy().is_unknown()
        {
            Some(SessionError::Authority)
        } else if self.preparing_reads.len() >= wes_budgets::get("storage.operations") as usize {
            Some(SessionError::Capacity)
        } else {
            None
        };
        if let Some(error) = refuse {
            let _ = reply.take().expect("read reply").send(Err(error));
            return true;
        }
        let Some(worker) = self.values.as_ref().map(|values| values.worker().clone()) else {
            let _ = reply
                .take()
                .expect("read reply")
                .send(Err(SessionError::NoStorage));
            return true;
        };
        self.maintain_residency(captured.snapshot.charge);
        let reply = reply.take().expect("read reply");
        let member = member.to_owned();
        let read_epoch = self.workspace.dataset_access.providers.epoch();
        self.preparing_reads.spawn(async move {
            let result = async {
                let mut selection = captured.snapshot.selection.clone();
                selection.extend_from_slice(&fields);
                let selected = worker
                    .describe(captured.snapshot.handle.clone(), selection)
                    .await?
                    .ok_or(StoreError::MissingValue)?;
                if selected.digest != captured.snapshot.digest {
                    return Err(StoreError::Conflict);
                }
                worker.materialize(selected).await
            }
            .await;
            ReadCompletion {
                read_epoch,
                output,
                captured,
                member,
                cooperative,
                result,
                reply,
            }
        });
        true
    }
    pub(super) fn complete_read(&mut self, read: ReadCompletion) {
        let result = (|| {
            if self.workspace.runtime().is_closed() {
                return Err(SessionError::Stopped);
            }
            if !read
                .captured
                .snapshot
                .provenance
                .policy()
                .provider_reads()
                .is_empty()
                && read.read_epoch != self.workspace.dataset_access.providers.epoch()
            {
                return Err(SessionError::HistoricalReplyUnavailable);
            }
            if self
                .workspace
                .dataset_access
                .blocks_policy(read.captured.snapshot.provenance.policy())
            {
                return Err(saved::store_error(StoreError::DatasetWithdrawn));
            }
            if !self.workspace.runtime().output(&read.output).accessible()
                || self
                    .workspace
                    .runtime()
                    .stored_output(&read.output.node)
                    .as_ref()
                    != Some(&read.captured)
            {
                return Err(SessionError::HistoricalReplyUnavailable);
            }
            let (loaded, _hold) = read.result.map_err(saved::store_error)?;
            if self.workspace.dataset_access.blocks(&loaded.value) {
                return Err(saved::store_error(StoreError::DatasetWithdrawn));
            }
            if read.cooperative
                && (loaded.value.provenance().policy().is_private()
                    || loaded.value.provenance().policy().is_unknown())
            {
                return Err(SessionError::Authority);
            }
            let value = loaded.value;
            crate::value_size::data_charge(value.data(), 4 * 1024 * 1024)
                .ok_or(SessionError::Capacity)?;
            Ok(sandbox::Observed {
                data: value.data().clone(),
                sources: sandbox::observe_sources(
                    &self.workspace,
                    self.values.as_ref(),
                    Some(&read.member),
                    false,
                ),
            })
        })();
        let _ = read.reply.send(result);
    }
    pub(super) fn maintain_residency(&mut self, reservation: u64) {
        let Some(values) = &self.values else {
            return;
        };
        let memory = values.worker().resident_memory().clone();
        let candidates = values
            .snapshot()
            .outputs
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let nodes = self
            .workspace
            .runtime()
            .graph()
            .nodes()
            .map(|n| n.id().clone())
            .collect::<Vec<_>>();
        for node in nodes {
            let eligible = self.workspace.runtime().value_of(&node).is_some_and(|v| {
                !v.shape().contains_meta()
                    && !v.provenance().policy().is_private()
                    && !v.provenance().policy().is_unknown()
            });
            if !eligible {
                continue;
            }
            let admit = |workspace: &Workspace| {
                workspace
                    .runtime()
                    .value_of(&node)
                    .is_none_or(|v| memory.admit(v).is_ok())
            };
            if !admit(&self.workspace) {
                for candidate in &candidates {
                    if candidate != &node {
                        self.workspace.release_resident_output(candidate);
                    }
                    if admit(&self.workspace) {
                        break;
                    }
                }
                if !admit(&self.workspace) {
                    self.workspace.release_resident_output(&node);
                }
            }
        }
        for candidate in candidates {
            if memory.used().saturating_add(reservation) <= memory.limit() {
                break;
            }
            self.workspace.release_resident_output(&candidate);
        }
    }
}
