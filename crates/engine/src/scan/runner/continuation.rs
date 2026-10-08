//! Inert continuation facts shared by preview and explicit admission.
use super::durable::Recipe;
use super::*;
use crate::scan::Totals;
use crate::storage::datasets::{
    AnalysisCheckpoint, AnalysisStatus, AnalysisStop, charge_interruption,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationReason {
    NotLatest,
    ActiveWriter,
    Finished,
    Deterministic,
    CumulativeStop,
    IncompleteSource,
    Lowered,
    AboveCeiling,
    FrozenAboveCeiling,
    Unchanged,
    IneffectiveRaise,
    NoHeadroom,
}
impl ContinuationReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::NotLatest => "not_latest",
            Self::ActiveWriter => "active_writer",
            Self::Finished => "finished",
            Self::Deterministic => "deterministic_stop",
            Self::CumulativeStop => "cumulative_stop",
            Self::IncompleteSource => "incomplete_source",
            Self::Lowered => "lowered",
            Self::AboveCeiling => "above_ceiling",
            Self::FrozenAboveCeiling => "frozen_above_ceiling",
            Self::Unchanged => "unchanged",
            Self::IneffectiveRaise => "ineffective_raise",
            Self::NoHeadroom => "no_headroom",
        }
    }
}
pub struct ContinuationReview {
    pub basis: String,
    pub requested: Totals,
    pub active: Settings,
    pub charged_work: u64,
    pub charged_duration_ms: u64,
    pub allowance_after: u64,
    pub resume_reason: Option<ContinuationReason>,
    pub continue_reason: Option<ContinuationReason>,
    pub(super) recipe: Recipe,
}
impl Recipe {
    pub(super) fn load(checkpoint: &AnalysisCheckpoint, span: Span) -> Result<Self, Failure> {
        let invalid = || Failure::new("CAL004", span, "captured analysis recipe is invalid");
        if checkpoint.captured_program.len() > wes_budgets::get("scan.code.bytes") as usize {
            return Err(invalid());
        }
        let recipe: Self =
            serde_json::from_str(&checkpoint.captured_program).map_err(|_| invalid())?;
        let coverage_policy = recipe
            .framing
            .as_ref()
            .and_then(|profile| match profile.malformed {
                wes_core::framing::Malformed::Strict {} => None,
                wes_core::framing::Malformed::Forensic { excerpt_bytes } => {
                    Some(crate::storage::datasets::CoveragePolicy {
                        excerpt_bytes: excerpt_bytes as u32,
                    })
                }
            });
        if recipe
            .framing
            .as_ref()
            .is_some_and(|profile| !profile.valid())
            || coverage_policy != checkpoint.coverage.as_ref().map(|c| c.policy)
            || checkpoint.coverage.as_ref().is_some_and(|c| {
                recipe.live
                    || !c.valid(
                        checkpoint.next_ordinal,
                        checkpoint.next_position,
                        checkpoint.usage.input_bytes,
                    )
            })
            || (recipe.framing.is_some()
                && (checkpoint.next_position != checkpoint.usage.input_bytes
                    || checkpoint.next_ordinal > checkpoint.next_position))
            || (recipe.framing.is_none() && checkpoint.next_position != checkpoint.next_ordinal)
        {
            return Err(invalid());
        }
        let mut revision = sha2::Sha256::new();
        use sha2::Digest;
        for binding in [
            &checkpoint.captured_program,
            &checkpoint.source.digest,
            &checkpoint.profile_digest,
        ] {
            revision.update((binding.len() as u64).to_le_bytes());
            revision.update(binding.as_bytes());
        }
        if !checkpoint.budget.valid()
            || checkpoint.budget.analysis != checkpoint.analysis
            || recipe.version != 3
            || recipe.live != checkpoint.followed_source.is_some()
            || recipe.identity.analysis != checkpoint.analysis
            || recipe.identity.profile_revision != checkpoint.profile_digest
            || !recipe.settings.restore(checkpoint.budget.totals).valid()
            || format!("sha256:{:x}", revision.finalize()) != checkpoint.task_revision
        {
            return Err(invalid());
        }
        Ok(recipe)
    }
}
impl ContinuationReview {
    pub(crate) fn frozen_settings(&self) -> Settings {
        self.recipe.settings.restore(self.requested)
    }
    pub fn new(
        reference: &wes_core::DatasetRef,
        checkpoint: &AnalysisCheckpoint,
        status: &AnalysisStatus,
        requested: Totals,
        active: Settings,
        span: Span,
    ) -> Result<Self, Failure> {
        use ContinuationReason::*;
        let recipe = Recipe::load(checkpoint, span)?;
        let (charged_work, charged_duration_ms) =
            charge_interruption(&checkpoint.work, &checkpoint.duration)
                .ok_or_else(|| Failure::new("CAL004", span, "captured analysis debit overflow"))?;
        let allowance_after = requested
            .work
            .checked_sub(checkpoint.budget.totals.work)
            .and_then(|delta| checkpoint.usage.work_allowance.checked_add(delta))
            .map_or(checkpoint.usage.work_allowance, |n| n.min(requested.work));
        let common = if !status.latest {
            Some(NotLatest)
        } else if status.active_writer {
            Some(ActiveWriter)
        } else if checkpoint.finish_applied
            || status.lifecycle == crate::storage::datasets::DatasetLifecycle::Sealed
        {
            Some(Finished)
        } else if !recipe.settings.restore(active.totals()).within(active) {
            Some(FrozenAboveCeiling)
        } else {
            None
        };
        let resume_reason = common.or_else(|| {
            if !checkpoint.budget.totals.within(active.totals()) {
                Some(AboveCeiling)
            } else if checkpoint
                .stop
                .is_some_and(|s| s != AnalysisStop::Cancelled)
            {
                Some(match checkpoint.stop {
                    Some(AnalysisStop::IncompleteSource) => IncompleteSource,
                    Some(AnalysisStop::Cumulative(_)) => CumulativeStop,
                    _ => Deterministic,
                })
            } else if !headroom(
                checkpoint,
                checkpoint.budget.totals,
                charged_work,
                charged_duration_ms,
                checkpoint.usage.work_allowance,
            ) || reference.records() >= checkpoint.budget.totals.output_records
            {
                Some(NoHeadroom)
            } else {
                None
            }
        });
        let continue_reason = common.or_else(|| {
            if !requested.within(active.totals()) {
                Some(AboveCeiling)
            } else if !requested.contains(checkpoint.budget.totals) {
                Some(Lowered)
            } else if requested == checkpoint.budget.totals {
                Some(Unchanged)
            } else if checkpoint.stop == Some(AnalysisStop::Deterministic) {
                Some(Deterministic)
            } else if checkpoint.stop == Some(AnalysisStop::IncompleteSource) {
                Some(IncompleteSource)
            } else if let Some(AnalysisStop::Cumulative(d)) = checkpoint.stop
                && !AnalysisStop::Cumulative(d).permits_raise(requested, checkpoint.budget.totals)
            {
                Some(IneffectiveRaise)
            } else if !headroom(
                checkpoint,
                requested,
                charged_work,
                charged_duration_ms,
                allowance_after,
            ) || reference.records() >= requested.output_records
            {
                Some(NoHeadroom)
            } else {
                None
            }
        });
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"wes.analysis.continuation.v1\0");
        let budget_digest = checkpoint.budget.digest();
        for field in [
            reference.manifest_digest(),
            checkpoint.attempt.as_str(),
            budget_digest.as_str(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        hash.update(serde_json::to_vec(&active).expect("captured settings"));
        for total in requested.values() {
            hash.update(total.to_le_bytes());
        }
        Ok(Self {
            basis: format!("sha256:{:x}", hash.finalize()),
            requested,
            active,
            charged_work,
            charged_duration_ms,
            allowance_after,
            resume_reason,
            continue_reason,
            recipe,
        })
    }
}
fn headroom(
    cp: &AnalysisCheckpoint,
    totals: Totals,
    work: u64,
    duration: u64,
    allowance: u64,
) -> bool {
    work < totals.work
        && work < allowance
        && duration < totals.duration_ms
        && cp.usage.input_bytes < totals.input_bytes
        && cp.next_ordinal < totals.input_records
        && cp.usage.output_bytes < totals.output_bytes
}
