//! Scope expiry removes temporary ownership; exact saved/shared prefixes remain reachable.
use super::*;
use wes_engine::storage::DatasetLifetime;

impl DatasetStore {
    pub(super) fn scope_is_open(&self, lifetime: &DatasetLifetime) -> bool {
        match lifetime {
            DatasetLifetime::Durable => true,
            DatasetLifetime::Transient(id) => self.scopes.get(id).is_some_and(|w| w.is_open()),
        }
    }
    pub(super) fn check_lifetime(
        &self,
        prefix: &DatasetRef,
        lifetime: &DatasetLifetime,
    ) -> Result<(), DatasetError> {
        if let DatasetLifetime::Transient(id) = lifetime {
            if self
                .scope
                .as_ref()
                .is_some_and(|scope| scope.id() == id && scope.is_open())
            {
                return Ok(());
            }
            if !self.references.values().any(|root| {
                root.lifetime == DatasetLifetime::Durable
                    && matches!(
                        root.retention,
                        RootRetention::Protected | RootRetention::Automatic
                    )
                    && root.prefixes.contains(prefix)
            }) {
                return Err(DatasetError::Unavailable);
            }
        }
        Ok(())
    }
    pub(super) fn payload_required(
        &self,
        prefix: &DatasetRef,
        lifetime: &DatasetLifetime,
        roots: &BTreeMap<String, ReferenceRootChange>,
    ) -> bool {
        self.scope_is_open(lifetime)
            || roots.values().any(|root| {
                root.lifetime == DatasetLifetime::Durable
                    && matches!(
                        root.retention,
                        RootRetention::Protected
                            | RootRetention::Automatic
                            | RootRetention::Unknown
                    )
                    && root.prefixes.contains(prefix)
            })
    }
    pub(super) fn current_lifetime(&self) -> DatasetLifetime {
        self.scope
            .as_ref()
            .map_or(DatasetLifetime::Durable, |scope| {
                DatasetLifetime::Transient(scope.id().into())
            })
    }
    pub(super) fn expire_scopes(&mut self) -> Result<(), DatasetError> {
        self.ready()?;
        let readers = self
            .readers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let writers = self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut expired = self
            .references
            .values()
            .filter(|root| {
                !self.scope_is_open(&root.lifetime)
                    && root.retention == RootRetention::Temporary
                    && !root.prefixes.is_empty()
                    && !root.prefixes.iter().any(|prefix| {
                        readers.contains(prefix) || writers.contains(prefix.dataset())
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        for batch in expired.chunks_mut(self.limits.catalog.roots_per_frame) {
            let transaction = uuid::Uuid::new_v4().to_string();
            for root in batch.iter_mut() {
                root.expected_generation = root.generation;
                root.generation = root
                    .generation
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("scope retirement"))?;
                root.transaction = transaction.clone();
                root.prefixes.clear();
                // The cleanup inventory retains capture identities until value cleanup confirms.
            }
            self.update_references(&transaction, batch, &FlowPolicy::default())?;
        }
        Ok(())
    }
}
