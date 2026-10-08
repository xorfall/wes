//! Continuations and their source protection share the dataset publication transaction.
use super::super::checkpoint::{
    Checkpoint, CheckpointBindings, InlineSnapshot, WorkLedger, digest,
};
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use wes_engine::storage::datasets::{AnalysisCheckpoint, AnalysisWork, CapturedValue};

impl DatasetStore {
    pub(super) fn publish_analysis_checkpoint(
        &mut self,
        manifest: &Manifest,
        state: &AnalysisCheckpoint,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, DatasetError> {
        if let Some(followed) = &state.followed_source {
            let source = self.read_exact(&followed.prefix)?;
            if source.kind != super::super::DatasetKind::EventLog
                || source
                    .recording
                    .as_ref()
                    .map(|r| (&r.run, &r.epoch, r.first))
                    != Some((&followed.run, &followed.epoch, followed.first))
                || followed.prefix.records() != manifest.source.end
                || source
                    .origins
                    .iter()
                    .any(|origin| !policy.origins().contains(origin))
                || source
                    .dataset_reads
                    .iter()
                    .any(|origin| !policy.dataset_reads().contains(origin))
            {
                return Err(DatasetError::Conflict);
            }
        }
        if state.coverage.as_ref() != manifest.coverage.as_ref().map(|c| &c.progress) {
            return Err(DatasetError::Conflict);
        }
        let prior = manifest
            .previous
            .as_ref()
            .map(|r| self.files.read_manifest(r, None))
            .transpose()?
            .and_then(|m| m.checkpoint)
            .map(|r| self.files.read_checkpoint(&r, &manifest.dataset, None))
            .transpose()?;
        let mut schema = |bundle: &ResolvedContractBundle,
                          previous: Option<(&ObjectRef, &str)>|
         -> Result<ObjectRef, DatasetError> {
            if let Some((reference, digest)) = previous {
                if bundle.digest() != digest {
                    return Err(DatasetError::Conflict);
                }
                Ok(reference.clone())
            } else {
                Ok(self.files.publish_schema(bundle, policy)?)
            }
        };
        let state_schema = schema(
            &state.state_schema,
            prior
                .as_ref()
                .map(|p| (&p.state.schema, p.state.schema_digest.as_str())),
        )?;
        let context_schema = schema(
            &state.context_schema,
            prior
                .as_ref()
                .map(|p| (&p.context.schema, p.context.schema_digest.as_str())),
        )?;
        let item_schema = schema(
            &state.item_schema,
            prior.as_ref().map(|p| {
                (
                    &p.bindings.item_schema,
                    p.bindings.item_schema_digest.as_str(),
                )
            }),
        )?;
        let limits = self.limits.objects.checkpoint;
        let state_snapshot =
            InlineSnapshot::capture(&state.state, state_schema, &state.state_schema, limits)?;
        let context_snapshot = InlineSnapshot::capture(
            &state.context,
            context_schema,
            &state.context_schema,
            limits,
        )?;
        let checkpoint = Checkpoint {
            coverage: state.coverage.clone(),
            stop: state.stop,
            budget: state.budget.clone(),
            version: 4,
            store: manifest.store.clone(),
            dataset: manifest.dataset.clone(),
            analysis: state.analysis.clone(),
            attempt: state.attempt.clone(),
            previous_attempt: state.previous_attempt.clone(),
            run: state.run.clone(),
            transaction: manifest.transaction.clone(),
            followed_source: state.followed_source.clone(),
            bindings: CheckpointBindings {
                task_revision: state.task_revision.clone(),
                source_handle: state.source.handle.clone(),
                source_digest: state.source.digest.clone(),
                source_bytes: state.source.bytes,
                source: manifest.source.clone(),
                item_schema,
                item_schema_digest: state.item_schema.digest().into(),
                output_schema: manifest.schema.clone(),
                output_schema_digest: manifest.schema_digest.clone(),
                step_revision: state.step_revision.clone(),
                finish_revision: state.finish_revision.clone(),
                language_version: 1,
                native_version: "wes.calc.native.v1".into(),
                profile_digest: state.profile_digest.clone(),
                initial_digest: if state.initial_digest.is_empty() && prior.is_none() {
                    state_snapshot.value_digest.clone()
                } else {
                    state.initial_digest.clone()
                },
                captured_program: state.captured_program.clone(),
                program_digest: digest(state.captured_program.as_bytes()),
            },
            state: state_snapshot,
            context: context_snapshot,
            next_position: state.next_position,
            next_ordinal: state.next_ordinal,
            output_end: manifest.summary.end,
            decoder_carry: STANDARD.encode(&state.decoder_carry),
            work: WorkLedger {
                granted: state.work.granted,
                completed: state.work.completed,
                charged: state.work.charged,
                outstanding: state.work.outstanding,
                grants: state.work.grants,
            },
            usage: state.usage.clone(),
            duration: state.duration.clone(),
            finish_applied: state.finish_applied,
            lifecycle: manifest.lifecycle,
            origins: manifest.origins.clone(),
            dataset_reads: manifest.dataset_reads.clone(),
        };
        Ok(self.files.publish_checkpoint(&checkpoint, policy)?)
    }

    pub(super) fn read_analysis_checkpoint(
        &self,
        reference: &DatasetRef,
    ) -> Result<Option<AnalysisCheckpoint>, DatasetError> {
        let manifest = self.read_exact(reference)?;
        let Some(reference) = manifest.checkpoint else {
            return Ok(None);
        };
        let saved = self
            .files
            .read_checkpoint(&reference, &manifest.dataset, None)?;
        let state_schema = self.files.read_schema(&saved.state.schema, None)?;
        let context_schema = self.files.read_schema(&saved.context.schema, None)?;
        let item_schema = self.files.read_schema(&saved.bindings.item_schema, None)?;
        let state = saved
            .state
            .value(&state_schema, self.limits.objects.checkpoint)?;
        let context = saved
            .context
            .value(&context_schema, self.limits.objects.checkpoint)?;
        Ok(Some(AnalysisCheckpoint {
            coverage: saved.coverage.clone(),
            stop: saved.stop,
            budget: saved.budget,
            analysis: saved.analysis,
            attempt: saved.attempt,
            previous_attempt: saved.previous_attempt,
            run: saved.run,
            task_revision: saved.bindings.task_revision,
            followed_source: saved.followed_source,
            source: CapturedValue {
                handle: saved.bindings.source_handle,
                digest: saved.bindings.source_digest,
                bytes: saved.bindings.source_bytes,
            },
            item_schema,
            state_schema,
            context_schema,
            state,
            context,
            step_revision: saved.bindings.step_revision,
            finish_revision: saved.bindings.finish_revision,
            profile_digest: saved.bindings.profile_digest,
            initial_digest: saved.bindings.initial_digest,
            captured_program: saved.bindings.captured_program,
            next_position: saved.next_position,
            next_ordinal: saved.next_ordinal,
            decoder_carry: STANDARD
                .decode(saved.decoder_carry)
                .map_err(|_| DatasetError::StorageCorrupt)?,
            work: AnalysisWork {
                granted: saved.work.granted,
                completed: saved.work.completed,
                charged: saved.work.charged,
                outstanding: saved.work.outstanding,
                grants: saved.work.grants,
            },
            usage: saved.usage,
            duration: saved.duration,
            finish_applied: saved.finish_applied,
        }))
    }

    pub(super) fn commit_roots(
        &self,
        manifest: &Manifest,
        reference: &ObjectRef,
    ) -> Result<Vec<ReferenceRootChange>, DatasetError> {
        if manifest.recording.is_some() {
            // Record is an explicit protected-retention intent. Advance its exact
            // prefix with the manifest, never through a later Keep acknowledgement.
            let previous = self.references.get(&manifest.dataset);
            if previous.is_none() && self.references.len() >= self.limits.reference_roots {
                return Err(DatasetError::Limit("reference roots"));
            }
            let expected_generation = previous.map_or(0, |root| root.generation);
            let change = ReferenceRootChange {
                root: manifest.dataset.clone(),
                kind: catalog::RootKind::Recording,
                owner_dataset: None,
                owner_workspace: None,
                expected_generation,
                generation: expected_generation
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("recording root generation"))?,
                prefixes: vec![descriptor(reference, manifest)?],
                captures: vec![],
                retention: RootRetention::Protected,
                transaction: manifest.transaction.clone(),
            };
            self.check_reference_successor(&change, previous)?;
            return Ok(vec![change]);
        }
        let Some(checkpoint) = &manifest.checkpoint else {
            return Ok(vec![]);
        };
        let saved = self
            .files
            .read_checkpoint(checkpoint, &manifest.dataset, None)?;
        let mut identities = vec![saved.analysis.clone()];
        if saved.run != saved.analysis {
            identities.push(saved.run.clone());
        }
        let mut changes = Vec::with_capacity(identities.len());
        let additions = identities
            .iter()
            .filter(|id| !self.references.contains_key(*id))
            .count();
        if self
            .references
            .len()
            .checked_add(additions)
            .is_none_or(|n| n > self.limits.reference_roots)
        {
            return Err(DatasetError::Limit("reference roots"));
        }
        for identity in identities {
            let previous = self.references.get(&identity);
            let expected_generation = previous.map_or(0, |r| r.generation);
            let change = ReferenceRootChange {
                root: identity,
                kind: catalog::RootKind::Checkpoint,
                owner_dataset: Some(manifest.dataset.clone()),
                owner_workspace: None,
                expected_generation,
                generation: expected_generation
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("checkpoint root generation"))?,
                prefixes: std::iter::once(descriptor(reference, manifest)?)
                    .chain(
                        saved
                            .followed_source
                            .as_ref()
                            .map(|source| source.prefix.clone()),
                    )
                    .collect(),
                captures: vec![CapturedValue {
                    handle: saved.bindings.source_handle.clone(),
                    digest: saved.bindings.source_digest.clone(),
                    bytes: saved.bindings.source_bytes,
                }],
                retention: RootRetention::Protected,
                transaction: manifest.transaction.clone(),
            };
            self.check_reference_successor(&change, previous)?;
            changes.push(change);
        }
        Ok(changes)
    }
}
