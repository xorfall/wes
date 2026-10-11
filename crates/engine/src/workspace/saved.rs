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
        if let Some(value) =
            alias_value(&saved, &values).map_err(|_| WorkspaceError::UnsafeObservation)?
        {
            if Some(value.shape()) != saved.alias_shape.as_ref() {
                return Err(WorkspaceError::UnsafeObservation);
            }
            self.install_saved_alias(&saved, Some(value), false)?;
        }
        self.saved_selections.push(saved);
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
