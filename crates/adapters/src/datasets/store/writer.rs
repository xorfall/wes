//! Shared typed writer: both analysis and event recording use the same catalog commit.
use super::super::{
    DatasetKind, IndexEntry, IndexSummary, PositionUnit, Record, SegmentHeader, SourceRange,
};
use super::*;
use wes_engine::storage::datasets::{
    DatasetAppend, DatasetCreate, DatasetKind as Kind, DatasetLifecycle, SourceExtent, SourceUnit,
};

fn source(extent: SourceExtent) -> SourceRange {
    SourceRange {
        identity: extent.identity,
        unit: match extent.unit {
            SourceUnit::Bytes => PositionUnit::Bytes,
            SourceUnit::Records => PositionUnit::Records,
        },
        start: extent.start,
        end: extent.end,
    }
}
fn lifecycle(value: DatasetLifecycle) -> Result<Lifecycle, DatasetError> {
    Ok(match value {
        DatasetLifecycle::Prefix => return Err(DatasetError::Conflict),
        DatasetLifecycle::Open => Lifecycle::Open,
        DatasetLifecycle::Sealed => Lifecycle::Sealed,
        DatasetLifecycle::Incomplete => Lifecycle::Incomplete,
        DatasetLifecycle::Interrupted => Lifecycle::Interrupted,
        DatasetLifecycle::Cancelled => Lifecycle::Cancelled,
        DatasetLifecycle::Restricted => Lifecycle::Restricted,
        DatasetLifecycle::Deleted => Lifecycle::Deleted,
    })
}
impl DatasetStore {
    fn check_new_reference_capacity(&self, identities: &[&str]) -> Result<(), DatasetError> {
        let additions = identities
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| !self.references.contains_key(*id))
            .count();
        if self
            .references
            .len()
            .checked_add(additions)
            .is_none_or(|count| count > self.limits.reference_roots)
        {
            return Err(DatasetError::Limit("reference roots"));
        }
        Ok(())
    }

    pub(super) fn create_owned(
        &mut self,
        request: DatasetCreate,
    ) -> Result<wes_engine::storage::datasets::DatasetWriterAdmission, DatasetError> {
        self.ready()?;
        self.check_policy_reads(request.policy.dataset_reads())?;
        if request.policy.is_private() || request.policy.is_unknown() {
            return Err(DatasetError::Restricted);
        }
        if !catalog::valid_uuid(&request.dataset) || !catalog::valid_uuid(&request.transaction) {
            return Err(DatasetError::Conflict);
        }
        if self.roots.contains_key(&request.dataset) || self.roots.len() >= self.limits.datasets {
            return Err(DatasetError::Conflict);
        }
        let source = source(request.source);
        super::super::tree::validate_source(&source)?;
        if request.checkpoint.as_ref().is_some_and(|cp| {
            request.owner.as_ref().is_some_and(|owner| {
                owner.run != cp.run
                    || owner.lineage != cp.analysis
                    || owner.role != wes_engine::storage::datasets::DatasetWriteRole::Analysis
            })
        }) {
            return Err(DatasetError::Conflict);
        }
        let identities = if request.recording.is_some() {
            vec![request.dataset.as_str()]
        } else {
            request
                .checkpoint
                .as_ref()
                .map(|cp| vec![cp.analysis.as_str(), cp.run.as_str()])
                .unwrap_or_default()
        };
        self.check_new_reference_capacity(&identities)?;
        let kind = match request.kind {
            Kind::Analysis => DatasetKind::Analysis,
            Kind::EventLog => DatasetKind::EventLog,
        };
        self.admit_write(
            request.owner.as_ref(),
            &request.transaction,
            &request.dataset,
            None,
            kind,
            catalog::WriteOperation::Create,
        )?;
        let schema = self
            .files
            .publish_schema(&request.schema, &request.policy)?;
        let persistence = match self.files.durability() {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        };
        let mut manifest = Manifest {
            version: 2,
            store: self.store_id().into(),
            dataset: request.dataset.clone(),
            kind: match request.kind {
                Kind::Analysis => DatasetKind::Analysis,
                Kind::EventLog => DatasetKind::EventLog,
            },
            generation: 1,
            previous: None,
            ancestors: vec![],
            transaction: request.transaction,
            schema,
            schema_digest: request.schema.digest().into(),
            index: None,
            summary: IndexSummary {
                first: 0,
                end: 0,
                segment_bytes: 0,
            },
            source,
            lifecycle: Lifecycle::Open,
            checkpoint: None,
            recording: request.recording,
            requested: persistence,
            established: persistence,
            authorization_generation: 1,
            origins: request.policy.origins().iter().cloned().collect(),
            dataset_reads: request.policy.dataset_reads().iter().cloned().collect(),
        };
        if let Some(checkpoint) = &request.checkpoint {
            if manifest.kind != DatasetKind::Analysis {
                return Err(DatasetError::Conflict);
            }
            manifest.checkpoint =
                Some(self.publish_analysis_checkpoint(&manifest, checkpoint, &request.policy)?);
        }
        self.commit(&manifest, &request.policy)?;
        let reference = self.descriptor(&request.dataset)?;
        self.admit_writer(reference)
    }
    pub(super) fn append_owned(
        &mut self,
        request: DatasetAppend,
    ) -> Result<DatasetRef, DatasetError> {
        self.ready()?;
        self.check_policy_reads(request.policy.dataset_reads())?;
        if request.policy.is_private() || request.policy.is_unknown() {
            return Err(DatasetError::Restricted);
        }
        if !catalog::valid_uuid(&request.transaction) {
            return Err(DatasetError::Conflict);
        }
        let mut manifest = self.read_exact(&request.previous)?;
        let (previous, current) = self
            .root(request.previous.dataset())?
            .ok_or(DatasetError::Unavailable)?;
        if descriptor(&previous, &current)? != request.previous
            || !self
                .active_writers
                .lock()
                .map_err(|_| DatasetError::StorageCorrupt)?
                .contains_key(request.previous.dataset())
            || manifest.lifecycle != Lifecycle::Open
            || manifest.checkpoint.is_some() != request.checkpoint.is_some()
            || manifest.recording.is_some() != request.recording.is_some()
            || manifest.transaction == request.transaction
        {
            return Err(DatasetError::Conflict);
        }
        let source = source(request.source);
        super::super::tree::validate_source(&source)?;
        if source.identity != manifest.source.identity
            || source.unit != manifest.source.unit
            || source.start != manifest.source.start
            || source.end < manifest.source.end
            || manifest
                .origins
                .iter()
                .any(|o| !request.policy.origins().contains(o))
            || manifest
                .dataset_reads
                .iter()
                .any(|o| !request.policy.dataset_reads().contains(o))
        {
            return Err(DatasetError::Conflict);
        }
        let next_lifecycle = lifecycle(request.lifecycle)?;
        if matches!(next_lifecycle, Lifecycle::Restricted | Lifecycle::Deleted) {
            return Err(DatasetError::Conflict);
        }
        if request.rows.len() > self.limits.objects.segment.records {
            return Err(DatasetError::Limit("batch records"));
        }
        let schema = self.files.read_schema(&manifest.schema)?;
        let count = request.rows.len() as u64;
        let first = manifest.summary.end;
        let mut records = Vec::with_capacity(request.rows.len());
        for (offset, row) in request.rows.into_iter().enumerate() {
            if row
                .value
                .provenance()
                .policy()
                .origins()
                .iter()
                .any(|o| !request.policy.origins().contains(o))
                || row
                    .value
                    .provenance()
                    .policy()
                    .dataset_reads()
                    .iter()
                    .any(|o| !request.policy.dataset_reads().contains(o))
            {
                return Err(DatasetError::Restricted);
            }
            if first.checked_add(offset as u64) != Some(row.ordinal) {
                return Err(DatasetError::Conflict);
            }
            records.push(Record {
                source_start: row.source_start,
                source_end: row.source_end,
                value: row.value,
            });
        }
        if request.checkpoint.as_ref().is_some_and(|cp| {
            request.owner.as_ref().is_some_and(|owner| {
                owner.run != cp.run
                    || owner.lineage != cp.analysis
                    || owner.role != wes_engine::storage::datasets::DatasetWriteRole::Analysis
            })
        }) {
            return Err(DatasetError::Conflict);
        }
        if manifest.kind == DatasetKind::EventLog
            && self
                .writes
                .get(&manifest.dataset)
                .is_some_and(|prior| request.owner.as_ref() != Some(&prior.owner))
        {
            return Err(DatasetError::Conflict);
        }
        self.admit_write(
            request.owner.as_ref(),
            &request.transaction,
            &manifest.dataset,
            Some(&request.previous),
            manifest.kind,
            catalog::WriteOperation::Append,
        )?;
        if count != 0 {
            let segment_source = SourceRange {
                identity: source.identity.clone(),
                unit: source.unit,
                start: records.first().unwrap().source_start,
                end: records.last().unwrap().source_end,
            };
            let header = SegmentHeader {
                store: self.store_id().into(),
                dataset: manifest.dataset.clone(),
                schema: schema.digest().into(),
                first,
                count,
                source: segment_source.clone(),
            };
            let segment =
                self.files
                    .publish_segment(&header, &records, &schema, &request.policy)?;
            let end = first
                .checked_add(count)
                .ok_or(DatasetError::Limit("ordinal"))?;
            let (index, summary) = self.files.append_index(
                manifest.index.as_ref(),
                &manifest.dataset,
                IndexEntry {
                    segment: segment.clone(),
                    summary: IndexSummary {
                        first,
                        end,
                        segment_bytes: segment.bytes,
                    },
                    source: segment_source,
                },
                &request.policy,
            )?;
            manifest.index = Some(index);
            manifest.summary = summary;
        }
        manifest.previous = Some(previous);
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or(DatasetError::Limit("generation"))?;
        manifest.ancestors = self.ancestry_links(
            &manifest.dataset,
            manifest.generation,
            manifest.previous.as_ref(),
        )?;
        manifest.transaction = request.transaction;
        manifest.source = source;
        manifest.lifecycle = next_lifecycle;
        manifest.recording = request.recording;
        manifest.origins = request.policy.origins().iter().cloned().collect();
        manifest.dataset_reads = request.policy.dataset_reads().iter().cloned().collect();
        if let Some(checkpoint) = &request.checkpoint {
            manifest.checkpoint =
                Some(self.publish_analysis_checkpoint(&manifest, checkpoint, &request.policy)?);
        }
        self.commit(&manifest, &request.policy)?;
        if manifest.lifecycle != Lifecycle::Open {
            self.active_writers
                .lock()
                .map_err(|_| DatasetError::StorageCorrupt)?
                .remove(&manifest.dataset);
        }
        self.descriptor(&manifest.dataset)
    }
    pub(super) fn resume_owned(
        &mut self,
        request: wes_engine::storage::datasets::DatasetResume,
    ) -> Result<wes_engine::storage::datasets::DatasetWriterAdmission, DatasetError> {
        self.ready()?;
        self.check_policy_reads(request.policy.dataset_reads())?;
        if request.policy.is_private() || request.policy.is_unknown() {
            return Err(DatasetError::Restricted);
        }
        let (previous, mut manifest) = self
            .root(request.previous.dataset())?
            .ok_or(DatasetError::Unavailable)?;
        self.read_exact(&request.previous)?;
        if descriptor(&previous, &manifest)? != request.previous {
            let head = descriptor(&previous, &manifest)?;
            if let Some(current) = self.read_analysis_checkpoint(&head)? {
                if request.checkpoint.previous_attempt.as_deref() != Some(current.attempt.as_str())
                {
                    return Err(DatasetError::NewerAttempt);
                }
            }
            return Err(DatasetError::Conflict);
        }
        if self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .contains_key(&manifest.dataset)
            || manifest.kind != DatasetKind::Analysis
            || !matches!(
                manifest.lifecycle,
                Lifecycle::Open
                    | Lifecycle::Incomplete
                    | Lifecycle::Cancelled
                    | Lifecycle::Interrupted
            )
            || !catalog::valid_uuid(&request.transaction)
            || request.transaction == manifest.transaction
        {
            return Err(DatasetError::Conflict);
        }
        let old = self
            .read_analysis_checkpoint(&request.previous)?
            .ok_or(DatasetError::Conflict)?;
        let new = &request.checkpoint;
        if !new.resumes(&old)
            || manifest
                .origins
                .iter()
                .any(|o| !request.policy.origins().contains(o))
            || manifest
                .dataset_reads
                .iter()
                .any(|o| !request.policy.dataset_reads().contains(o))
        {
            return Err(DatasetError::Conflict);
        }
        self.check_new_reference_capacity(&[new.analysis.as_str(), new.run.as_str()])?;
        let owner = wes_engine::storage::datasets::DatasetWriteOwner {
            role: wes_engine::storage::datasets::DatasetWriteRole::Analysis,
            run: new.run.clone(),
            lineage: new.analysis.clone(),
        };
        self.admit_write(
            Some(&owner),
            &request.transaction,
            &manifest.dataset,
            Some(&request.previous),
            manifest.kind,
            catalog::WriteOperation::Resume,
        )?;
        manifest.previous = Some(previous);
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or(DatasetError::Limit("generation"))?;
        manifest.ancestors = self.ancestry_links(
            &manifest.dataset,
            manifest.generation,
            manifest.previous.as_ref(),
        )?;
        manifest.transaction = request.transaction;
        manifest.lifecycle = Lifecycle::Open;
        manifest.origins = request.policy.origins().iter().cloned().collect();
        manifest.dataset_reads = request.policy.dataset_reads().iter().cloned().collect();
        manifest.checkpoint =
            Some(self.publish_analysis_checkpoint(&manifest, new, &request.policy)?);
        self.commit(&manifest, &request.policy)?;
        let reference = self.descriptor(&manifest.dataset)?;
        self.admit_writer(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_only_prefix_cannot_become_a_writer_transition() {
        assert!(matches!(
            lifecycle(DatasetLifecycle::Prefix),
            Err(DatasetError::Conflict)
        ));
        assert_eq!(lifecycle(DatasetLifecycle::Open).unwrap(), Lifecycle::Open);
        assert_eq!(
            lifecycle(DatasetLifecycle::Sealed).unwrap(),
            Lifecycle::Sealed
        );
    }
}
