//! One exact capture, protected copies, then the visible immutable journal record.
use super::*;
use crate::{
    graph::OutputPort,
    history::{SavedEdge, SavedLineage, SavedMember, SavedPayload, SavedSelection},
    storage::ValueLease,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Capture {
    pub scope: String,
    pub members: Vec<SavedMember>,
    pub lineage: Vec<SavedLineage>,
    pub values: BTreeMap<String, wes_core::Value>,
    pub holds: Vec<ValueLease>,
}
impl Actor {
    pub(super) fn capture_saved(&self, member: Option<&str>) -> Result<Capture, SessionError> {
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        let storage = self.values.as_ref().ok_or(SessionError::NoStorage)?;
        let worker = storage.worker();
        let scope = worker
            .transient_identity()
            .ok_or(SessionError::Authority)?
            .to_owned();
        let published = storage.snapshot();
        let mut capture = Capture {
            scope,
            members: vec![],
            lineage: vec![],
            values: BTreeMap::new(),
            holds: vec![],
        };
        let selected = member.map(|m| m.split('.').next().unwrap());
        let fields = member.map_or_else(Vec::new, |m| {
            m.split('.').skip(1).map(str::to_owned).collect()
        });
        let mut roots = Vec::new();
        let mut remaining = wes_budgets::get("values.pending.bytes");
        for (name, output) in self.workspace.bindings().names() {
            if selected.is_some_and(|n| n != name)
                || (selected.is_none() && self.workspace.native_inputs.contains(&output.node))
            {
                continue;
            }
            let node = self
                .workspace
                .runtime()
                .graph()
                .node(&output.node)
                .ok_or(SessionError::UnknownNode)?;
            let typing = self
                .workspace
                .typing(output)
                .ok_or(SessionError::UnknownValue)?;
            if typing.provenance.policy().is_private() || typing.provenance.policy().is_unknown() {
                return Err(SessionError::Authority);
            }
            let mut value = match self.workspace.runtime().output(output) {
                crate::runtime::OutputState::Available(value) => Some(value),
                _ => None,
            };
            let evidence = if output.port == OutputPort::Data && value.is_none() {
                self.workspace
                    .runtime()
                    .evidence_value(&output.node)
                    .map(|e| {
                        value = Some(e.value.clone());
                        e.kind.name().to_owned()
                    })
            } else {
                None
            };
            let run = self
                .workspace
                .runtime()
                .run_of(&output.node)
                .map(ToString::to_string);
            let mut payload = None;
            let mut selected_typing = typing.shape.to_string();
            if let Some(value) = value {
                if value.provenance().policy().is_private()
                    || value.provenance().policy().is_unknown()
                    || self.workspace.dataset_access.blocks(&value)
                {
                    return Err(SessionError::Authority);
                }
                if output.port == OutputPort::Data
                    && value.data().is_storable_snapshot()
                    && !value.shape().contains_meta()
                {
                    let publication = published
                        .outputs
                        .get(&output.node)
                        .filter(|p| Some(p.run().as_str()) == run.as_deref())
                        .ok_or(SessionError::UnknownValue)?;
                    let handle = publication
                        .handle()
                        .cloned()
                        .ok_or(SessionError::CheckpointBusy)?;
                    if matches!(publication, ValuePublication::Complete(p) if p.problem.is_some()) {
                        return Err(SessionError::UnknownValue);
                    }
                    let projected = crate::plan::project_value(&value, &fields)
                        .map_err(|_| SessionError::UnknownValue)?;
                    let charge = crate::value_size::value_charge(&projected, remaining)
                        .ok_or(SessionError::Capacity)?;
                    remaining -= charge;
                    payload = Some(SavedPayload {
                        handle: handle.to_string(),
                        digest: String::new(),
                    });
                    selected_typing = projected.shape().to_string();
                    capture.values.insert(name.clone(), projected);
                } else if selected.is_some() {
                    return Err(SessionError::UnknownValue);
                }
            } else if selected.is_some() {
                return Err(SessionError::UnknownValue);
            }
            roots.push(output.node.clone());
            capture.members.push(SavedMember {
                name: name.clone(),
                node: output.node.to_string(),
                run,
                revision: self.workspace.runtime().display_revision(&output.node),
                port: output.port.selector().into(),
                state: format!("{:?}", node.state()).to_lowercase(),
                typing: selected_typing,
                operation: match node.payload() {
                    BoundTask::Scan(_) => "scan",
                    BoundTask::ScanAttempt(_) => "scan_attempt",
                    BoundTask::Call(_) => "call",
                    BoundTask::Calculation(_) => "calculation",
                    BoundTask::Input(_) | BoundTask::SavedInput(_) => "input",
                    BoundTask::View(_) => "view",
                    _ => "other",
                }
                .into(),
                fields: fields.clone(),
                payload,
                evidence,
            });
        }
        if selected.is_some() && capture.members.is_empty() {
            return Err(SessionError::UnknownValue);
        }
        let mut visited = BTreeSet::new();
        while let Some(id) = roots.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            if visited.len() > wes_budgets::get("values.outputs") as usize {
                return Err(SessionError::Capacity);
            }
            let node = self
                .workspace
                .runtime()
                .graph()
                .node(&id)
                .ok_or(SessionError::UnknownNode)?;
            roots.extend(node.dependencies().keys().cloned());
            capture.lineage.push(SavedLineage {
                node: id.to_string(),
                run: self
                    .workspace
                    .runtime()
                    .run_of(&id)
                    .map(ToString::to_string),
                revision: self.workspace.runtime().display_revision(&id),
                origin: self.workspace.native_origins.get(&id).cloned(),
                inputs: node
                    .dependencies()
                    .iter()
                    .map(|(node, port)| SavedEdge {
                        node: node.to_string(),
                        run: self
                            .workspace
                            .runtime()
                            .run_of(node)
                            .map(ToString::to_string),
                        revision: self.workspace.runtime().display_revision(node),
                        port: port.selector().into(),
                    })
                    .collect(),
            });
        }
        Ok(capture)
    }
}
impl Capture {
    pub(super) async fn protect(
        &mut self,
        worker: &crate::storage::StoreWorker,
    ) -> Result<(), SessionError> {
        let handles = self
            .members
            .iter()
            .filter_map(|m| m.payload.as_ref())
            .map(|p| crate::storage::ValueHandle::new(&p.handle))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(store_error)?;
        self.holds = worker
            .hold_many(handles.into_iter().collect())
            .await
            .map_err(store_error)?;
        let mut digests: BTreeMap<String, String> = BTreeMap::new();
        for member in &mut self.members {
            if let Some(payload) = &mut member.payload {
                if let Some(digest) = digests.get(&payload.handle) {
                    payload.digest = digest.clone();
                    continue;
                }
                let bytes = worker
                    .encoded(
                        crate::storage::ValueHandle::new(&payload.handle).map_err(store_error)?,
                    )
                    .await
                    .map_err(store_error)?
                    .ok_or(SessionError::UnknownValue)?;
                payload.digest = format!("sha256:{:x}", Sha256::digest(&bytes));
                digests.insert(payload.handle.clone(), payload.digest.clone());
            }
        }
        Ok(())
    }
}
pub(crate) fn alias_value(
    saved: &SavedSelection,
    values: &BTreeMap<String, wes_core::Value>,
) -> Result<Option<wes_core::Value>, SessionError> {
    if saved.single {
        return Ok(values.get(&saved.members[0].name).cloned());
    }
    let shape = wes_core::Shape::Record(
        wes_core::RecordShape::new(
            "Record",
            values.iter().map(|(n, v)| (n.clone(), v.shape().clone())),
        )
        .map_err(|_| SessionError::Preparation)?,
    );
    let data = wes_core::Data::Record(
        values
            .iter()
            .map(|(n, v)| (n.clone(), v.data().clone()))
            .collect(),
    );
    let provenance =
        wes_core::Provenance::agreed_by(values.values().map(wes_core::Value::provenance));
    let value =
        wes_core::Value::new(shape, data, provenance).map_err(|_| SessionError::Preparation)?;
    if crate::value_size::value_charge(&value, wes_budgets::get("values.pending.bytes")).is_none() {
        return Err(SessionError::Capacity);
    }
    Ok(Some(value))
}
pub(crate) async fn load_alias(
    worker: &crate::storage::StoreWorker,
    saved: &SavedSelection,
) -> Result<Option<wes_core::Value>, SessionError> {
    let _holds = worker
        .hold_many(saved.handles())
        .await
        .map_err(store_error)?;
    let mut remaining = wes_budgets::get("values.pending.bytes");
    let mut values = BTreeMap::new();
    for member in &saved.members {
        if member.payload.is_none() {
            continue;
        }
        let value = load_member(worker, member, &[]).await?;
        remaining = remaining
            .checked_sub(
                crate::value_size::value_charge(&value, remaining).ok_or(SessionError::Capacity)?,
            )
            .ok_or(SessionError::Capacity)?;
        values.insert(member.name.clone(), value);
    }
    let value = alias_value(saved, &values)?;
    if value.as_ref().map(wes_core::Value::shape) != saved.alias_shape.as_ref() {
        return Err(SessionError::UnknownValue);
    }
    Ok(value)
}
pub(super) fn store_error(error: crate::storage::StoreError) -> SessionError {
    use crate::storage::StoreError;
    match error {
        StoreError::Limit(_) => SessionError::Capacity,
        StoreError::Restricted => SessionError::Authority,
        StoreError::ScopeUnavailable => SessionError::ScopeEnded,
        StoreError::Closed => SessionError::Stopped,
        _ => SessionError::UnknownValue,
    }
}
pub(super) async fn load_member(
    worker: &crate::storage::StoreWorker,
    member: &SavedMember,
    fields: &[String],
) -> Result<wes_core::Value, SessionError> {
    let payload = member.payload.as_ref().ok_or(SessionError::UnknownValue)?;
    let handle = crate::storage::ValueHandle::new(&payload.handle).map_err(store_error)?;
    let _hold = worker
        .hold(handle.clone())
        .await
        .map_err(store_error)?
        .ok_or(SessionError::UnknownValue)?;
    let loaded = worker
        .recover(handle.clone())
        .await
        .map_err(store_error)?
        .ok_or(SessionError::UnknownValue)?;
    let bytes = worker
        .encoded(handle)
        .await
        .map_err(store_error)?
        .ok_or(SessionError::UnknownValue)?;
    if format!("sha256:{:x}", Sha256::digest(&bytes)) != payload.digest {
        return Err(SessionError::UnknownValue);
    }
    let mut path = member.fields.clone();
    path.extend_from_slice(fields);
    crate::plan::project_value(&loaded.loaded.value, &path).map_err(|_| SessionError::UnknownValue)
}
pub(crate) fn alias_id(saved: &SavedSelection) -> NodeId {
    NodeId::new(format!("saved:{}", saved.id)).expect("saved id")
}
