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
        if request.coverage.is_some_and(|policy| !policy.valid())
            || request.coverage.is_some()
                && (request.kind != Kind::Analysis
                    || source.unit != PositionUnit::Bytes
                    || request.checkpoint.is_none()
                    || request
                        .checkpoint
                        .as_ref()
                        .is_some_and(|cp| cp.followed_source.is_some()))
            || request
                .checkpoint
                .as_ref()
                .and_then(|cp| cp.coverage.as_ref())
                .map(|c| c.policy)
                != request.coverage
            || request
                .checkpoint
                .as_ref()
                .and_then(|cp| cp.coverage.as_ref())
                .is_some_and(|c| c.records != 0 || !c.valid(0, 0, 0))
        {
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
        let coverage = request
            .coverage
            .map(|policy| {
                let schema = wes_engine::storage::datasets::rejection_schema();
                Ok::<_, DatasetError>(super::super::manifest::CoverageRoot {
                    schema: self.files.publish_schema(&schema, &request.policy)?,
                    schema_digest: schema.digest().into(),
                    index: None,
                    summary: IndexSummary {
                        coverage: None,
                        first: 0,
                        end: 0,
                        segment_bytes: 0,
                    },
                    progress: wes_engine::storage::datasets::CoverageProgress::empty(policy),
                })
            })
            .transpose()?;
        let persistence = match self.files.durability() {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        };
        let mut manifest = Manifest {
            coverage,
            version: 3,
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
                coverage: None,
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
        let schema = self.files.read_schema(&manifest.schema, None)?;
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
        if request.coverage.len() > self.limits.objects.segment.records {
            return Err(DatasetError::Limit("coverage batch records"));
        }
        let mut coverage = manifest.coverage.clone();
        let mut coverage_records = Vec::with_capacity(request.coverage.len());
        if let Some(root) = &mut coverage {
            let next = request.checkpoint.as_ref().ok_or(DatasetError::Conflict)?;
            let prior = manifest.checkpoint.as_ref().ok_or(DatasetError::Conflict)?;
            let prior = self.files.read_checkpoint(prior, &manifest.dataset, None)?;
            for row in &request.coverage {
                if row.source.start < prior.next_position
                    || row.ordinal < prior.next_ordinal
                    || (row.unterminated && row.delimiter.end != manifest.source.end)
                {
                    return Err(DatasetError::Conflict);
                }
                root.progress
                    .observe(row, next.next_ordinal, next.next_position)
                    .map_err(|_| DatasetError::Conflict)?;
                coverage_records.push(Record {
                    source_start: row.source.start,
                    source_end: row.delimiter.end,
                    value: wes_engine::storage::datasets::rejection_value(row, &request.policy)
                        .map_err(|_| DatasetError::Conflict)?,
                });
            }
            if next.coverage.as_ref() != Some(&root.progress) {
                return Err(DatasetError::Conflict);
            }
        } else if !request.coverage.is_empty()
            || request
                .checkpoint
                .as_ref()
                .is_some_and(|c| c.coverage.is_some())
        {
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
            let (index, summary) = self.publish_stream_rows(
                &manifest.dataset,
                super::super::Stream::Outputs,
                manifest.index.as_ref(),
                first,
                &schema,
                &source,
                &records,
                None,
                &request.policy,
            )?;
            manifest.index = Some(index);
            manifest.summary = summary;
        }
        if let Some(root) = &mut coverage {
            if !coverage_records.is_empty() {
                let schema = self.files.read_schema(&root.schema, None)?;
                let (index, summary) = self.publish_stream_rows(
                    &manifest.dataset,
                    super::super::Stream::Coverage,
                    root.index.as_ref(),
                    root.summary.end,
                    &schema,
                    &source,
                    &coverage_records,
                    Some(root.progress.policy),
                    &request.policy,
                )?;
                root.index = Some(index);
                root.summary = summary;
            }
        }
        manifest.coverage = coverage;
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
    #[allow(clippy::too_many_arguments)]
    fn publish_stream_rows(
        &mut self,
        dataset: &str,
        stream: super::super::Stream,
        prior: Option<&ObjectRef>,
        first: u64,
        schema: &ResolvedContractBundle,
        source: &SourceRange,
        records: &[Record],
        coverage_policy: Option<wes_engine::storage::datasets::CoveragePolicy>,
        policy: &FlowPolicy,
    ) -> Result<(ObjectRef, IndexSummary), DatasetError> {
        let count = records.len() as u64;
        let source = SourceRange {
            identity: source.identity.clone(),
            unit: source.unit,
            start: records.first().ok_or(DatasetError::Conflict)?.source_start,
            end: records.last().ok_or(DatasetError::Conflict)?.source_end,
        };
        let coverage = coverage_policy
            .map(|policy| {
                let mut span: Option<wes_engine::storage::datasets::CoverageSpan> = None;
                for row in records {
                    let row = wes_engine::storage::datasets::read_rejection(&row.value, policy)
                        .map_err(|_| DatasetError::StorageCorrupt)?;
                    let next = wes_engine::storage::datasets::CoverageSpan::single(&row)
                        .map_err(|_| DatasetError::Conflict)?;
                    match &mut span {
                        Some(span) => span.merge(&next).map_err(|_| DatasetError::Conflict)?,
                        None => span = Some(next),
                    }
                }
                Ok::<_, DatasetError>(super::super::SegmentCoverage {
                    policy,
                    span: span.ok_or(DatasetError::Conflict)?,
                })
            })
            .transpose()?;
        let header = SegmentHeader {
            coverage: coverage.clone(),
            store: self.store_id().into(),
            dataset: dataset.into(),
            stream,
            schema: schema.digest().into(),
            first,
            count,
            source: source.clone(),
        };
        let segment = self
            .files
            .publish_segment(&header, records, schema, policy)?;
        self.files
            .append_index(
                prior,
                dataset,
                stream,
                IndexEntry {
                    summary: IndexSummary {
                        coverage: coverage.map(|c| c.span),
                        first,
                        end: first
                            .checked_add(count)
                            .ok_or(DatasetError::Limit("stream ordinal"))?,
                        segment_bytes: segment.bytes,
                    },
                    segment,
                    source,
                },
                policy,
            )
            .map_err(DatasetError::from)
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
        use wes_engine::storage::datasets::AnalysisAttemptKind;
        let active = wes_engine::scan::Settings::capture().totals();
        if (request.kind == AnalysisAttemptKind::Continue
            && (!new.budget.totals.within(active) || new.budget.ceilings != active))
            || !new.continues(&old, request.kind)
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
            match request.kind {
                AnalysisAttemptKind::Resume => catalog::WriteOperation::Resume,
                AnalysisAttemptKind::Continue => catalog::WriteOperation::Continue,
            },
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
