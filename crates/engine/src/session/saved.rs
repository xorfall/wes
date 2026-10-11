//! One exact capture, protected copies, then the visible immutable journal record.
use super::*;
use crate::{
    graph::OutputPort,
    history::{SavedEdge, SavedLineage, SavedMember, SavedPayload, SavedSelection},
    storage::ValueLease,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Capture {
    pub scope: String,
    pub members: Vec<SavedMember>,
    pub lineage: Vec<SavedLineage>,
    pub values: BTreeMap<String, wes_core::Value>,
    pub shapes: BTreeMap<String, wes_core::Shape>,
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
            shapes: BTreeMap::new(),
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
            if !typing.provenance.policy().allows_retention() {
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
            let mut physical_fields = fields.clone();
            if let crate::runtime::OutputState::Stored(stored) =
                self.workspace.runtime().output(output)
            {
                if !stored.snapshot.provenance.policy().allows_retention() {
                    return Err(SessionError::Authority);
                }
                let shape = crate::plan::field_shape(&stored.snapshot.shape, &fields)
                    .ok_or(SessionError::UnknownValue)?;
                physical_fields = stored.snapshot.selection.clone();
                physical_fields.extend_from_slice(&fields);
                selected_typing = shape.to_string();
                payload = Some(SavedPayload {
                    handle: stored.snapshot.handle.to_string(),
                    digest: stored.snapshot.digest.clone(),
                });
                capture.shapes.insert(name.clone(), shape);
            } else if let Some(value) = value {
                if !value.provenance().policy().allows_retention()
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
                    capture
                        .shapes
                        .insert(name.clone(), projected.shape().clone());
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
                    BoundTask::Input(_) | BoundTask::StoredInput(_) | BoundTask::SavedInput(_) => {
                        "input"
                    }
                    BoundTask::View(_) => "view",
                    _ => "other",
                }
                .into(),
                fields: physical_fields,
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
                let snapshot = worker
                    .describe(
                        crate::storage::ValueHandle::new(&payload.handle).map_err(store_error)?,
                        vec![],
                    )
                    .await
                    .map_err(store_error)?
                    .ok_or(SessionError::UnknownValue)?;
                if !snapshot.provenance.policy().allows_retention() {
                    return Err(SessionError::Authority);
                }
                let digest = snapshot.digest.clone();
                if !payload.digest.is_empty() && payload.digest != digest {
                    return Err(store_error(crate::storage::StoreError::Conflict));
                }
                payload.digest = digest;
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
        return Err(store_error(crate::storage::StoreError::Conflict));
    }
    Ok(value)
}
pub(super) fn store_error(error: crate::storage::StoreError) -> SessionError {
    use crate::storage::StoreError;
    match error {
        StoreError::ScopeUnavailable => SessionError::ScopeEnded,
        error => SessionError::ResultRead {
            kind: error.read_failure_kind(),
            message: error.read_message(),
        },
    }
}

#[cfg(test)]
mod read_error_tests {
    use super::*;

    #[test]
    fn saved_read_errors_preserve_categories_and_safe_context_instead_of_unknown_handle() {
        use crate::storage::StoreError;
        for (error, code, public) in [
            (
                StoreError::Limit("fixture decoded input"),
                "RUN007",
                "fixture decoded input",
            ),
            (StoreError::Conflict, "RUN010", "captured identity"),
            (StoreError::Closed, "RUN011", "storage worker"),
            (StoreError::Corrupt, "RUN012", "validation failed"),
            (
                StoreError::DatasetUnconfirmed {
                    transaction: "fixture".into(),
                },
                "RUN013",
                "unconfirmed",
            ),
            (
                StoreError::backend(
                    "reading retained result",
                    std::io::Error::other("PRIVATE_SAVED_CAUSE"),
                ),
                "RUN014",
                "reading retained result",
            ),
        ] {
            let session = store_error(error);
            assert!(!matches!(session, SessionError::UnknownValue));
            let runtime = session.read_error("Saved snapshot read refused");
            assert_eq!(runtime.code(), code);
            assert!(runtime.message().to_lowercase().contains(public));
            assert_eq!(
                runtime
                    .message()
                    .matches("No provider was replayed")
                    .count(),
                1
            );
            assert!(!runtime.message().contains("PRIVATE_SAVED_CAUSE"));
        }
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
    let mut path = member.fields.clone();
    path.extend_from_slice(fields);
    let snapshot = worker
        .describe(handle, path)
        .await
        .map_err(store_error)?
        .ok_or(SessionError::UnknownValue)?;
    if snapshot.digest != payload.digest {
        return Err(store_error(crate::storage::StoreError::Conflict));
    }
    let (loaded, _resident_hold) = worker.materialize(snapshot).await.map_err(store_error)?;
    Ok(loaded.value)
}
pub(crate) fn alias_id(saved: &SavedSelection) -> NodeId {
    NodeId::new(format!("saved:{}", saved.id)).expect("saved id")
}

/// Reconstruct an inert stored alias from exact retained membership, without payload decoding.
pub(crate) async fn stored_alias(
    worker: &crate::storage::StoreWorker,
    saved: &SavedSelection,
) -> Result<Option<crate::storage::StoredSnapshot>, SessionError> {
    let Some(shape) = &saved.alias_shape else {
        return Ok(None);
    };
    let snapshot = if saved.single {
        let member = &saved.members[0];
        let payload = member.payload.as_ref().ok_or(SessionError::UnknownValue)?;
        let snapshot = worker
            .describe(
                crate::storage::ValueHandle::new(&payload.handle).map_err(store_error)?,
                member.fields.clone(),
            )
            .await
            .map_err(store_error)?
            .ok_or(SessionError::UnknownValue)?;
        if snapshot.digest != payload.digest {
            return Err(store_error(crate::storage::StoreError::Conflict));
        }
        snapshot
    } else {
        let members = saved
            .members
            .iter()
            .filter_map(|m| {
                m.payload.as_ref().map(|p| crate::storage::SnapshotMember {
                    name: m.name.clone(),
                    handle: p.handle.clone(),
                    digest: p.digest.clone(),
                    selection: m.fields.clone(),
                })
            })
            .collect();
        worker
            .compose(
                crate::storage::ValueHandle::new(&saved.id).map_err(store_error)?,
                members,
            )
            .await
            .map_err(store_error)?
    };
    if &snapshot.shape != shape {
        return Err(store_error(crate::storage::StoreError::Conflict));
    }
    Ok(Some(snapshot))
}
