//! Saved artifacts install inert typed input bindings, never provider calls.
use super::*;
use crate::{
    history::SavedSelection,
    session::saved::{alias_id, alias_value},
};
use std::collections::BTreeMap;
impl Workspace {
    pub(crate) fn install_saved_selection(
        &mut self,
        saved: SavedSelection,
        values: BTreeMap<String, wes_core::Value>,
        stored: Option<crate::storage::StoredSnapshot>,
    ) -> Result<(), WorkspaceError> {
        saved
            .validate()
            .map_err(|_| WorkspaceError::UnsafeObservation)?;
        if self
            .saved_selections
            .iter()
            .any(|s| s.id == saved.id || s.name == saved.name)
            || self.resolve(&saved.name).is_some()
        {
            return Err(WorkspaceError::UnsafeObservation);
        }
        if self.saved_selections.len() >= wes_budgets::get("values.outputs") as usize {
            return Err(WorkspaceError::UnsafeObservation);
        }
        if values.len() != saved.members.iter().filter(|m| m.payload.is_some()).count() {
            let snapshot = stored.ok_or(WorkspaceError::UnsafeObservation)?;
            self.install_stored_saved_alias(&saved, snapshot)?;
        } else if let Some(value) =
            alias_value(&saved, &values).map_err(|_| WorkspaceError::UnsafeObservation)?
        {
            if Some(value.shape()) != saved.alias_shape.as_ref() {
                return Err(WorkspaceError::UnsafeObservation);
            }
            self.install_saved_alias(&saved, Some(value), false)?;
            if let Some(snapshot) = stored {
                let run = crate::runtime::RunId::new(format!("saved:{}", saved.id))
                    .expect("validated saved identity");
                if !self.attach_stored_output(&alias_id(&saved), &run, snapshot) {
                    return Err(WorkspaceError::UnsafeObservation);
                }
            }
        }
        self.saved_selections.push(saved);
        Ok(())
    }
    pub(crate) fn install_stored_saved_alias(
        &mut self,
        saved: &SavedSelection,
        snapshot: crate::storage::StoredSnapshot,
    ) -> Result<(), WorkspaceError> {
        if saved.alias_shape.as_ref() != Some(&snapshot.shape) {
            return Err(WorkspaceError::UnsafeObservation);
        }
        let id = alias_id(saved);
        let typing = Arc::new(wes_core::capability::Typing {
            shape: snapshot.shape.clone(),
            provenance: snapshot.provenance.clone(),
        });
        self.runtime.restore(
            id.clone(),
            BoundTask::SavedInput(saved.clone()),
            [],
            crate::runtime::ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
            crate::runtime::RestoredState::ReadyStored(snapshot),
            Some(crate::runtime::RunId::new(format!("saved:{}", saved.id)).expect("saved id")),
        )?;
        self.predictions.insert(id.clone(), typing);
        self.bindings
            .bind(
                &saved.name,
                crate::graph::OutputRef::data(id.clone()),
                self.runtime.graph(),
            )
            .map_err(|_| WorkspaceError::UnsafeObservation)?;
        self.native_inputs.insert(id);
        Ok(())
    }
    pub(crate) fn install_saved_alias(
        &mut self,
        saved: &SavedSelection,
        value: Option<wes_core::Value>,
        held: bool,
    ) -> Result<(), WorkspaceError> {
        use crate::runtime::{ExecutionTraits, RestoredState};
        let Some(shape) = saved.alias_shape.as_ref() else {
            return Ok(());
        };
        let id = alias_id(saved);
        self.runtime.restore(
            id.clone(),
            BoundTask::SavedInput(saved.clone()),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
            if !held && value.is_some() {
                RestoredState::Ready(value.as_ref().unwrap().clone())
            } else {
                RestoredState::Stale
            },
            Some(crate::runtime::RunId::new(format!("saved:{}", saved.id)).expect("saved id")),
        )?;
        self.predictions.insert(
            id.clone(),
            Arc::new(wes_core::capability::Typing {
                shape: shape.clone(),
                provenance: value
                    .as_ref()
                    .map_or_else(Default::default, |v| v.provenance().clone()),
            }),
        );
        self.bindings
            .bind(
                &saved.name,
                crate::graph::OutputRef::data(id.clone()),
                self.runtime.graph(),
            )
            .map_err(|_| WorkspaceError::UnsafeObservation)?;
        self.native_inputs.insert(id);
        Ok(())
    }
}
