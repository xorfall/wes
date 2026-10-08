//! Semantic retention port translation; filesystem wire types stay in this adapter.
use super::*;
use wes_engine::storage::{
    Retention, StoreError,
    datasets::{DatasetRootKind, DatasetRootReceipt, DatasetRootRequest},
};

impl DatasetStore {
    pub(super) fn root_covers_owned(
        &self,
        identity: &wes_engine::storage::ValueHandle,
        prefixes: &[DatasetRef],
        retention: Retention,
    ) -> Result<bool, StoreError> {
        let Some(root) = self
            .reference_root(identity.as_str())
            .map_err(storage_error)?
        else {
            return Ok(false);
        };
        let required = match retention {
            Retention::Temporary => RootRetention::Temporary,
            Retention::Automatic => RootRetention::Automatic,
            Retention::Protected => RootRetention::Protected,
            Retention::Unknown => RootRetention::Unknown,
        };
        Ok(root.kind == catalog::RootKind::Value
            && root.prefixes == prefixes
            && root.retention == required)
    }
    pub(super) fn set_owned_root(
        &mut self,
        request: DatasetRootRequest,
    ) -> Result<DatasetRootReceipt, StoreError> {
        self.ready().map_err(storage_error)?;
        if request.kind == DatasetRootKind::Workspace {
            return Err(StoreError::Conflict);
        }
        let previous = self
            .reference_root(request.identity.as_str())
            .map_err(storage_error)?;
        let kind = match request.kind {
            DatasetRootKind::Value => catalog::RootKind::Value,
            DatasetRootKind::Workspace => catalog::RootKind::Workspace,
            DatasetRootKind::Keep => catalog::RootKind::Keep,
            DatasetRootKind::Pin => catalog::RootKind::Pin,
            DatasetRootKind::Checkpoint => catalog::RootKind::Checkpoint,
            DatasetRootKind::Recording => catalog::RootKind::Recording,
        };
        let retention = match request.retention {
            Retention::Temporary => RootRetention::Temporary,
            Retention::Automatic => RootRetention::Automatic,
            Retention::Protected => RootRetention::Protected,
            Retention::Unknown => RootRetention::Unknown,
        };
        if let Some(previous) = &previous {
            if previous.kind == kind
                && previous.prefixes == request.prefixes
                && previous.captures == request.captures
                && previous.retention == retention
            {
                // Confirm persistence even when the chosen root is already installed.
                self.update_references(
                    &previous.transaction,
                    std::slice::from_ref(previous),
                    &request.policy,
                )
                .map_err(storage_error)?;
                return Ok(DatasetRootReceipt {
                    transaction: previous.transaction.clone(),
                    generation: previous.generation,
                    persistence: self.root_persistence(),
                });
            }
        }
        let expected_generation = previous.as_ref().map_or(0, |r| r.generation);
        let change = ReferenceRootChange {
            root: request.identity.to_string(),
            kind,
            owner_dataset: previous
                .as_ref()
                .and_then(|root| root.owner_dataset.clone()),
            owner_workspace: None,
            expected_generation,
            generation: expected_generation
                .checked_add(1)
                .ok_or(StoreError::Limit("reference generation"))?,
            prefixes: request.prefixes,
            captures: request.captures,
            retention,
            transaction: uuid::Uuid::new_v4().to_string(),
        };
        self.update_references(
            &change.transaction,
            std::slice::from_ref(&change),
            &request.policy,
        )
        .map_err(storage_error)?;
        Ok(DatasetRootReceipt {
            transaction: change.transaction,
            generation: change.generation,
            persistence: self.root_persistence(),
        })
    }
    fn root_persistence(&self) -> wes_engine::history::Persistence {
        match self.files.durability() {
            Durability::File => wes_engine::history::Persistence::FileSynced,
            Durability::FileAndDirectory => {
                wes_engine::history::Persistence::FileAndDirectorySynced
            }
        }
    }
}
