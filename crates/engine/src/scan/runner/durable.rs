//! Durable admission uses the same pure runner and original cumulative limits.
use super::*;
use crate::storage::datasets::{
    AnalysisCheckpoint, AnalysisDuration, AnalysisUsage, AnalysisWork, CapturedValue,
    DatasetAppend, DatasetLifecycle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wes_core::contracts::ResolvedContractBundle;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recipe {
    pub version: u16,
    pub live: bool,
    pub step: String,
    pub finish: Option<String>,
    pub settings: Settings,
    pub framing: Option<Profile>,
    pub identity: Identity,
}
pub(super) struct Durable {
    pub checkpoint: AnalysisCheckpoint,
    pub grant_start_work: u64,
    pub storage_failed: bool,
}
fn grant_cap(settings: Settings) -> u64 {
    // One complete bounded input/invocation/validation handoff, including native framing and
    // catalog charges. An expanding earned allowance cannot turn this into an unbounded grant.
    settings
        .record_charge
        .saturating_mul(2)
        .saturating_add(settings.state_charge.saturating_mul(2))
        .saturating_add(settings.scratch.bytes)
        .saturating_add(settings.scratch.work)
        .saturating_add((settings.block as u64).saturating_mul(64))
        .saturating_add(65536)
        .min(settings.limits.work)
}
/// A restored runner cannot be polled until its new attempt and prepaid grant are acknowledged.
pub struct PreparedResume {
    runner: Runner,
    request: crate::storage::datasets::DatasetResume,
}
impl PreparedResume {
    pub fn request(&self) -> crate::storage::datasets::DatasetResume {
        self.request.clone()
    }
    pub fn acknowledge(mut self, reference: wes_core::DatasetRef) -> Result<Runner, Failure> {
        let prior = &self.request.previous;
        if reference.store() != prior.store()
            || reference.dataset() != prior.dataset()
            || reference.schema_digest() != prior.schema_digest()
            || reference.records() != prior.records()
            || reference.authorization_generation() != prior.authorization_generation()
            || reference.generation() != prior.generation().checked_add(1).unwrap_or(0)
        {
            return Err(self
                .runner
                .durable_failure("resume acknowledgement does not match its admitted attempt"));
        }
        let baseline = self.request.checkpoint.work.charged;
        self.runner.dataset = Some(reference);
        self.runner.durable = Some(Durable {
            checkpoint: self.request.checkpoint,
            grant_start_work: baseline,
            storage_failed: false,
        });
        Ok(self.runner)
    }
}
impl Runner {
    /// Reconstruct solely from protected captured input/program/contracts, never the current
    /// definition or provider. Preparation is bounded and inert; publication grants execution.
    pub fn prepare_resume(
        source: Value,
        follow: bool,
        reference: wes_core::DatasetRef,
        checkpoint: AnalysisCheckpoint,
        run: String,
        pool: &MemoryPool,
        services: Option<Arc<dyn LocalServices>>,
        token: CancellationToken,
        span: Span,
    ) -> Result<PreparedResume, Failure> {
        let invalid = || {
            Failure::new(
                "CAL004",
                span,
                "captured analysis cannot be resumed under its original contracts and limits",
            )
        };
        let cap = wes_budgets::get("scan.code.bytes") as usize;
        if checkpoint.captured_program.len() > cap
            || checkpoint.finish_applied
            || !checkpoint.decoder_carry.is_empty()
            || uuid::Uuid::parse_str(&run).is_err()
            || run == checkpoint.run
        {
            return Err(invalid());
        }
        let recipe: Recipe =
            serde_json::from_str(&checkpoint.captured_program).map_err(|_| invalid())?;
        if (follow && !recipe.live)
            || recipe.version != 2
            || recipe.live != checkpoint.followed_source.is_some()
            || recipe.identity.analysis != checkpoint.analysis
            || recipe.identity.profile_revision != checkpoint.profile_digest
            || !recipe.settings.within(Settings::capture())
        {
            return Err(invalid());
        }
        let mut revision = Sha256::new();
        for binding in [
            &checkpoint.captured_program,
            &checkpoint.source.digest,
            &checkpoint.profile_digest,
        ] {
            revision.update((binding.len() as u64).to_le_bytes());
            revision.update(binding.as_bytes());
        }
        if format!("sha256:{:x}", revision.finalize()) != checkpoint.task_revision {
            return Err(invalid());
        }
        let step = Transition::restore_program(&recipe.step, cap, span)?;
        let finish = recipe
            .finish
            .as_ref()
            .map(|text| Transition::restore_program(text, cap, span))
            .transpose()?;
        if step.revision() != checkpoint.step_revision
            || finish.as_ref().map(|t| t.revision()) != checkpoint.finish_revision.as_deref()
            || step.state.digest() != checkpoint.state_schema.digest()
            || step.context.digest() != checkpoint.context_schema.digest()
            || step.input.digest() != checkpoint.item_schema.digest()
            || step.output_contract.digest() != reference.schema_digest()
            || checkpoint.work.limit != recipe.settings.limits.work
        {
            return Err(invalid());
        }
        let mut checkpoint = checkpoint;
        if !checkpoint.duration.valid()
            || checkpoint.duration.limit_ms != recipe.settings.duration.as_millis() as u64
        {
            return Err(invalid());
        }
        checkpoint.duration.spent_ms = checkpoint
            .duration
            .spent_ms
            .checked_add(checkpoint.duration.outstanding_ms)
            .ok_or_else(invalid)?;
        if checkpoint.duration.spent_ms >= checkpoint.duration.limit_ms {
            return Err(Failure::new(
                "CAL006",
                span,
                "original analysis duration is exhausted; an interrupted interval is charged conservatively and resume cannot mint time",
            ));
        }
        checkpoint.duration.outstanding_ms =
            checkpoint.duration.limit_ms - checkpoint.duration.spent_ms;
        checkpoint.work.charged = checkpoint
            .work
            .charged
            .checked_add(checkpoint.work.outstanding)
            .ok_or_else(invalid)?;
        let remaining = checkpoint
            .usage
            .work_allowance
            .checked_sub(checkpoint.work.charged)
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                Failure::new(
                    "CAL006",
                    span,
                    "original analysis work allowance is exhausted; resume cannot mint work",
                )
            })?
            .min(grant_cap(recipe.settings));
        checkpoint.work.outstanding = remaining;
        checkpoint.work.granted = checkpoint
            .work
            .granted
            .checked_add(remaining)
            .ok_or_else(invalid)?;
        checkpoint.work.grants = checkpoint.work.grants.checked_add(1).ok_or_else(invalid)?;
        checkpoint.previous_attempt = Some(checkpoint.attempt.clone());
        checkpoint.attempt = uuid::Uuid::new_v4().to_string();
        checkpoint.run = run;
        let usage = Usage {
            work: checkpoint.work.charged,
            input_bytes: checkpoint.usage.input_bytes,
            input_records: checkpoint.next_ordinal,
            held_bytes: 0,
            high_water_bytes: checkpoint.usage.high_water_bytes,
            output_bytes: checkpoint.usage.output_bytes,
            output_records: reference.records(),
        };
        let ledger = Ledger::restored(
            recipe.settings.limits,
            pool,
            usage,
            checkpoint.usage.work_allowance,
        )
        .map_err(|e| refusal(e, span).failure)?;
        ledger
            .prepaid(
                checkpoint
                    .work
                    .charged
                    .checked_add(remaining)
                    .ok_or_else(invalid)?,
            )
            .map_err(|e| refusal(e, span).failure)?;
        let source = if let Some(followed) = &checkpoint.followed_source {
            Value::new(
                source.shape().clone(),
                Data::Dataset(followed.prefix.clone().into()),
                source.provenance().clone(),
            )
            .map_err(|_| invalid())?
            .with_metadata(source.metadata().cloned())
        } else {
            source
        };
        let mut runner = Self::new_admitted(
            Input {
                live: recipe.live && follow,
                source,
                initial: checkpoint.state.clone(),
                context: checkpoint.context.clone(),
                step,
                finish,
                framing: recipe.framing,
                identity: recipe.identity,
                control: None,
            },
            recipe.settings,
            ledger,
            services,
            token,
            span,
        )?;
        runner.elapsed_prior = Duration::from_millis(checkpoint.duration.spent_ms);
        if let Some(followed) = &checkpoint.followed_source {
            runner.source.follow(followed.clone(), span)?;
            if !follow {
                runner.source.stop_following();
            }
        }
        runner
            .source
            .restore_boundary(checkpoint.next_position, checkpoint.next_ordinal, span)?;
        runner.committed_position = checkpoint.next_position;
        runner.result_contract = result_contract_for_sink(&runner.step, true, span)?;
        runner.result_metadata = ValueMetadata::capture(&runner.result_contract);
        let request = crate::storage::datasets::DatasetResume {
            previous: reference,
            transaction: uuid::Uuid::new_v4().to_string(),
            checkpoint,
            policy: runner.provenance.policy().clone(),
        };
        Ok(PreparedResume { runner, request })
    }
    pub fn captured_source(&self) -> Value {
        self.source.value().clone()
    }
    pub fn checkpoint_seed(&self, source: CapturedValue) -> Result<AnalysisCheckpoint, Failure> {
        let limit = wes_budgets::get("scan.code.bytes") as usize;
        let recipe = Recipe {
            version: 2,
            live: self.live,
            step: self.step.captured_program(limit, self.span)?,
            finish: self
                .finish
                .as_ref()
                .map(|t| t.captured_program(limit, self.span))
                .transpose()?,
            settings: self.settings,
            framing: self.source.profile().cloned(),
            identity: self.identity.clone(),
        };
        let captured_program =
            crate::scan::captured::encode_program(&recipe, limit).map_err(|_| {
                Failure::new(
                    "CAL006",
                    self.span,
                    "captured scan recipe exceeds its code limit",
                )
            })?;
        let capture = |c| {
            ResolvedContractBundle::capture(c, Default::default())
                .map_err(|_| self.durable_failure("scan schema capture failed"))
        };
        let measured = self.ledger.usage().work;
        let mut revision = Sha256::new();
        for binding in [
            &captured_program,
            &source.digest,
            &self.identity.profile_revision,
        ] {
            revision.update((binding.len() as u64).to_le_bytes());
            revision.update(binding.as_bytes());
        }
        Ok(AnalysisCheckpoint {
            duration: AnalysisDuration {
                limit_ms: self.settings.duration.as_millis() as u64,
                spent_ms: self.elapsed_ms(),
                outstanding_ms: 0,
            },
            analysis: self.identity.analysis.clone(),
            attempt: uuid::Uuid::new_v4().to_string(),
            previous_attempt: None,
            run: self.identity.analysis.clone(),
            task_revision: format!("sha256:{:x}", revision.finalize()),
            source,
            followed_source: self.source.followed_source().cloned(),
            item_schema: capture(self.step.input.clone())?,
            state_schema: capture(self.step.state.clone())?,
            context_schema: capture(self.step.context.clone())?,
            state: self.state.clone(),
            context: self.context.clone(),
            step_revision: self.step.revision().into(),
            finish_revision: self.finish.as_ref().map(|t| t.revision().into()),
            profile_digest: self.identity.profile_revision.clone(),
            initial_digest: String::new(),
            captured_program,
            next_position: 0,
            next_ordinal: 0,
            decoder_carry: vec![],
            work: AnalysisWork {
                limit: self.settings.limits.work,
                granted: measured,
                completed: measured,
                charged: measured,
                outstanding: 0,
                grants: u64::from(measured != 0),
            },
            usage: AnalysisUsage {
                input_bytes: 0,
                output_bytes: 0,
                high_water_bytes: self.ledger.usage().high_water_bytes,
                work_allowance: self.ledger.work_allowance(),
            },
            finish_applied: false,
        })
    }
    pub fn attach_durable(
        &mut self,
        reference: wes_core::DatasetRef,
        checkpoint: AnalysisCheckpoint,
    ) -> Result<(), Failure> {
        if checkpoint.analysis != self.identity.analysis
            || checkpoint.step_revision != self.step.revision()
            || checkpoint.work.limit != self.settings.limits.work
            || checkpoint.work.outstanding != 0
            || checkpoint.next_position != 0
            || checkpoint.next_ordinal != 0
            || checkpoint.state.data() != self.state.data()
        {
            return Err(
                self.durable_failure("durable admission does not match its captured runner")
            );
        }
        self.attach_dataset(reference)?;
        self.ledger
            .prepaid(self.ledger.usage().work)
            .map_err(|e| refusal(e, self.span).failure)?;
        self.durable = Some(Durable {
            checkpoint,
            grant_start_work: self.ledger.usage().work,
            storage_failed: false,
        });
        Ok(())
    }
    pub fn needs_durable_grant(&self) -> bool {
        self.durable
            .as_ref()
            .is_some_and(|d| d.checkpoint.work.outstanding == 0)
            && self.candidate.is_none()
            && !matches!(
                self.phase,
                Phase::Complete | Phase::Stopped | Phase::Cancelled
            )
    }
    /// Returned grant is inert until the owner confirms its catalog publication.
    pub fn durable_grant(&self) -> Result<DatasetAppend, Failure> {
        if !self.needs_durable_grant() {
            return Err(self.durable_failure("scan does not need another work grant"));
        }
        let mut checkpoint = self.durable.as_ref().unwrap().checkpoint.clone();
        checkpoint.followed_source = self.source.followed_source().cloned();
        checkpoint.duration.spent_ms = self.elapsed_ms();
        checkpoint.duration.outstanding_ms = checkpoint
            .duration
            .limit_ms
            .saturating_sub(checkpoint.duration.spent_ms);
        if checkpoint.duration.outstanding_ms == 0 {
            return Err(Failure::new(
                "CAL006",
                self.span,
                "original analysis duration is exhausted",
            ));
        }
        let remaining = self
            .ledger
            .remaining_work()
            .min(grant_cap(self.settings))
            .min(
                checkpoint
                    .work
                    .limit
                    .saturating_sub(checkpoint.work.charged),
            );
        if remaining == 0 {
            return Err(Failure::new(
                "CAL006",
                self.span,
                "scan cumulative work cap is exhausted; resume cannot mint work",
            ));
        }
        checkpoint.work.outstanding = remaining;
        checkpoint.work.granted = checkpoint
            .work
            .granted
            .checked_add(remaining)
            .ok_or_else(|| self.durable_failure("scan grant counter overflow"))?;
        checkpoint.work.grants = checkpoint
            .work
            .grants
            .checked_add(1)
            .ok_or_else(|| self.durable_failure("scan grant counter overflow"))?;
        Ok(DatasetAppend {
            owner: Some(self.write_owner()),
            previous: self.dataset.clone().unwrap(),
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![],
            source: self.source_extent(),
            lifecycle: DatasetLifecycle::Open,
            policy: self.provenance.policy().clone(),
            checkpoint: Some(checkpoint),
            recording: None,
        })
    }
    pub fn acknowledge_grant(
        &mut self,
        reference: wes_core::DatasetRef,
        checkpoint: AnalysisCheckpoint,
    ) -> Result<(), Failure> {
        let prior = self
            .dataset
            .as_ref()
            .ok_or_else(|| self.durable_failure("scan has no dataset sink"))?;
        if !self.needs_durable_grant()
            || reference.store() != prior.store()
            || reference.dataset() != prior.dataset()
            || reference.schema_digest() != prior.schema_digest()
            || reference.authorization_generation() != prior.authorization_generation()
            || reference.generation() != prior.generation().checked_add(1).unwrap_or(0)
            || reference.records() != prior.records()
            || checkpoint.next_position != self.committed_position
            || checkpoint.work.outstanding == 0
        {
            return Err(self.durable_failure(
                "work grant acknowledgement does not match its admitted checkpoint",
            ));
        }
        self.dataset = Some(reference);
        self.phase = Phase::Reading;
        let durable = self.durable.as_mut().expect("durable admission");
        durable.checkpoint = checkpoint;
        durable.grant_start_work = self.ledger.usage().work;
        self.ledger
            .prepaid(
                durable
                    .grant_start_work
                    .checked_add(durable.checkpoint.work.outstanding)
                    .ok_or_else(|| {
                        Failure::new("CAL006", self.span, "scan prepaid work overflow")
                    })?,
            )
            .map_err(|e| refusal(e, self.span).failure)?;
        // Catalog/codec work belongs to the prepaid grant, before the next callback.
        self.ledger
            .work(4096)
            .map_err(|e| refusal(e, self.span).failure)?;
        Ok(())
    }
    pub(super) fn candidate_checkpoint(&self) -> Result<Option<AnalysisCheckpoint>, Failure> {
        match &self.candidate {
            Some(candidate) => self.checkpoint_for_candidate(candidate),
            None => Ok(None),
        }
    }
    pub(super) fn checkpoint_for_candidate(
        &self,
        candidate: &Candidate,
    ) -> Result<Option<AnalysisCheckpoint>, Failure> {
        let Some(durable) = &self.durable else {
            return Ok(None);
        };
        let usage = candidate.usage;
        let mut checkpoint = self.settled_checkpoint()?;
        checkpoint.state = candidate.state.clone();
        checkpoint.next_position = candidate
            .range
            .map_or(self.committed_position, |(_, end, _)| end);
        checkpoint.next_ordinal = usage.input_records;
        checkpoint.finish_applied = candidate.finishing;
        checkpoint.usage = AnalysisUsage {
            input_bytes: usage.input_bytes,
            output_bytes: usage.output_bytes,
            high_water_bytes: usage.high_water_bytes,
            work_allowance: durable
                .checkpoint
                .usage
                .work_allowance
                .saturating_add(candidate.range.map_or(0, |(_, _, charge)| {
                    charge.saturating_mul(self.settings.work_per_input_unit)
                }))
                .min(self.settings.limits.work),
        };
        Ok(Some(checkpoint))
    }
    fn settled_checkpoint(&self) -> Result<AnalysisCheckpoint, Failure> {
        let durable = self
            .durable
            .as_ref()
            .ok_or_else(|| self.durable_failure("scan has no durable checkpoint"))?;
        let mut checkpoint = durable.checkpoint.clone();
        checkpoint.followed_source = self.source.followed_source().cloned();
        checkpoint.duration.spent_ms = self.elapsed_ms();
        checkpoint.duration.outstanding_ms = 0;
        let measured = self
            .ledger
            .usage()
            .work
            .saturating_sub(durable.grant_start_work);
        if measured > checkpoint.work.outstanding {
            return Err(self.durable_failure("scan work exceeded its prepaid grant"));
        }
        checkpoint.work.completed = checkpoint
            .work
            .completed
            .checked_add(measured)
            .ok_or_else(|| self.durable_failure("scan measured work overflow"))?;
        checkpoint.work.charged = checkpoint
            .work
            .charged
            .checked_add(measured)
            .ok_or_else(|| self.durable_failure("scan charged work overflow"))?;
        checkpoint.work.outstanding = 0;
        Ok(checkpoint)
    }
    /// Terminal status preserves only the acknowledged state/output prefix.
    pub fn durable_terminal_update(&self) -> Result<Option<DatasetAppend>, Failure> {
        if self.durable.as_ref().is_none_or(|d| d.storage_failed) || self.phase == Phase::Complete {
            return Ok(None);
        }
        if !matches!(self.phase, Phase::Stopped | Phase::Cancelled) {
            return Err(self.durable_failure("scan is not terminal"));
        }
        let checkpoint = self.settled_checkpoint()?;
        Ok(Some(DatasetAppend {
            owner: Some(self.write_owner()),
            previous: self.dataset.clone().unwrap(),
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![],
            source: self.source_extent(),
            lifecycle: if self.phase == Phase::Cancelled {
                DatasetLifecycle::Cancelled
            } else {
                DatasetLifecycle::Incomplete
            },
            policy: self.provenance.policy().clone(),
            checkpoint: Some(checkpoint),
            recording: None,
        }))
    }
    pub fn acknowledge_terminal(
        &mut self,
        reference: wes_core::DatasetRef,
        checkpoint: AnalysisCheckpoint,
    ) -> Result<(), Failure> {
        let prior = self
            .dataset
            .as_ref()
            .ok_or_else(|| self.durable_failure("scan has no dataset sink"))?;
        if !matches!(self.phase, Phase::Stopped | Phase::Cancelled)
            || reference.store() != prior.store()
            || reference.dataset() != prior.dataset()
            || reference.schema_digest() != prior.schema_digest()
            || reference.authorization_generation() != prior.authorization_generation()
            || reference.generation() != prior.generation().checked_add(1).unwrap_or(0)
            || reference.records() != prior.records()
        {
            return Err(self.durable_failure("terminal checkpoint acknowledgement is inconsistent"));
        }
        self.dataset = Some(reference);
        let durable = self.durable.as_mut().expect("durable checkpoint");
        durable.checkpoint = checkpoint;
        durable.grant_start_work = self.ledger.usage().work;
        Ok(())
    }
    fn durable_failure(&self, message: &str) -> Failure {
        Failure::new("CAL004", self.span, message)
    }
}
