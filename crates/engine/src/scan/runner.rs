//! Finite pure analysis: one attempt, one cumulative ledger, a fresh VM per
//! record. This owner has no workspace, provider, disk or mutable registry port.
use super::{
    CapturedSource, SourcePoll, Transition, framing_charge,
    ledger::{Dimension, Ledger, Limits, MemoryPool, Refusal, Usage},
};
use crate::{
    calc::{self, Failure, LocalServices, Machine, Request},
    driver::CancellationToken,
};
use indexmap::IndexMap;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use wes_core::{
    Data, IterRegexCache, Provenance, Value,
    contracts::{Contract, ContractField, ContractKind, ContractRegistry, metadata::ValueMetadata},
    framing::{ByteSpan, Profile},
};
use wes_language::Span;

/// Immutable admission, captured before the attempt enters its source. All
/// charge fields are conservative logical accounting, not resident memory.
/// page_bytes independently bounds encoded rows; source positions retain their native unit.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub limits: Limits,
    pub startup_work: u64,
    pub work_per_input_unit: u64,
    pub source_charge: u64,
    pub state_charge: u64,
    pub context_charge: u64,
    pub record_charge: u64,
    pub scratch: calc::Limits,
    pub outputs_per_record: u64,
    pub patterns: usize,
    pub block: usize,
    pub duration: Duration,
    pub page_rows: usize,
    pub page_bytes: usize,
    pub page_segments: usize,
    pub commit_records: usize,
    pub commit_bytes: u64,
}
impl Settings {
    fn within(&self, ceiling: Self) -> bool {
        self.valid()
            && self.limits.work <= ceiling.limits.work
            && self.limits.input_bytes <= ceiling.limits.input_bytes
            && self.limits.input_records <= ceiling.limits.input_records
            && self.limits.memory_bytes <= ceiling.limits.memory_bytes
            && self.limits.output_bytes <= ceiling.limits.output_bytes
            && self.limits.output_records <= ceiling.limits.output_records
            && self.startup_work <= ceiling.startup_work
            && self.work_per_input_unit <= ceiling.work_per_input_unit
            && self.source_charge <= ceiling.source_charge
            && self.state_charge <= ceiling.state_charge
            && self.context_charge <= ceiling.context_charge
            && self.record_charge <= ceiling.record_charge
            && self.scratch.work <= ceiling.scratch.work
            && self.scratch.bytes <= ceiling.scratch.bytes
            && self.scratch.frames <= ceiling.scratch.frames
            && self.scratch.quantum <= ceiling.scratch.quantum
            && self.outputs_per_record <= ceiling.outputs_per_record
            && self.patterns <= ceiling.patterns
            && self.block <= ceiling.block
            && self.duration <= ceiling.duration
            && self.page_rows <= ceiling.page_rows
            && self.page_bytes <= ceiling.page_bytes
            && self.page_segments <= ceiling.page_segments
            && self.commit_records <= ceiling.commit_records
            && self.commit_bytes <= ceiling.commit_bytes
    }
    /// One immutable snapshot of the common application budget catalogue.
    pub fn capture() -> Self {
        let get = wes_budgets::get;
        let limits = Limits {
            work: get("scan.work"),
            input_bytes: get("scan.input.bytes"),
            input_records: get("scan.input.records"),
            memory_bytes: get("scan.memory.bytes"),
            output_bytes: get("scan.output.bytes"),
            output_records: get("scan.output.records"),
        };
        Self {
            limits,
            startup_work: get("scan.startup.work").min(limits.work),
            work_per_input_unit: get("scan.work.rate"),
            source_charge: get("scan.source.bytes"),
            state_charge: get("scan.state.bytes"),
            context_charge: get("scan.context.bytes"),
            record_charge: get("scan.record.bytes"),
            scratch: calc::Limits {
                work: get("scan.record.work"),
                bytes: get("scan.scratch.bytes"),
                frames: get("calc.frames") as usize,
                calls: 0,
                quantum: get("calc.quantum") as usize,
            },
            outputs_per_record: get("scan.record.outputs"),
            patterns: get("scan.patterns") as usize,
            block: get("scan.block.bytes") as usize,
            duration: Duration::from_millis(get("scan.duration.ms")),
            page_rows: 32.min(get("dataset.page.rows") as usize),
            page_bytes: get("scan.record.bytes").min(get("dataset.page.bytes")) as usize,
            page_segments: 8.min(get("dataset.page.segments") as usize),
            commit_records: get("scan.commit.records") as usize,
            commit_bytes: get("scan.commit.bytes"),
        }
    }
    pub fn valid(&self) -> bool {
        self.limits.valid()
            && self.limits.work <= 1_000_000_000
            && self.limits.memory_bytes <= 1024 * 1024 * 1024
            && self.limits.input_bytes <= i64::MAX as u64
            && self.limits.input_records <= i64::MAX as u64
            && self.limits.output_records <= i64::MAX as u64
            && self.limits.output_bytes <= i64::MAX as u64
            && self.startup_work > 0
            && self.startup_work <= self.limits.work
            && (1..=1_000_000).contains(&self.work_per_input_unit)
            && [
                self.source_charge,
                self.state_charge,
                self.context_charge,
                self.record_charge,
            ]
            .into_iter()
            .all(|n| n > 0 && n <= self.limits.memory_bytes)
            && self.scratch.bytes > 0
            && self.scratch.bytes <= self.limits.memory_bytes
            && self.scratch.work > 0
            && self.scratch.work <= 100_000_000
            && (1..=4096).contains(&self.scratch.quantum)
            && (1..=256).contains(&self.scratch.frames)
            && self.scratch.calls == 0
            && (1..=100_000).contains(&self.outputs_per_record)
            && (1..=64).contains(&self.patterns)
            && (1..=65_536).contains(&self.block)
            && !self.duration.is_zero()
            && self.duration <= Duration::from_secs(86_400)
            && (1..=32).contains(&self.page_rows)
            && (1..=16 * 1024 * 1024).contains(&self.page_bytes)
            && (1..=8).contains(&self.page_segments)
            && (1..=128).contains(&self.commit_records)
            && (1..=1024 * 1024).contains(&self.commit_bytes)
    }
}

pub struct Input {
    pub live: bool,
    pub source: Value,
    pub initial: Value,
    pub context: Value,
    pub step: Transition,
    pub finish: Option<Transition>,
    pub framing: Option<Profile>,
    pub identity: Identity,
    pub control: Option<Provenance>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub node: String,
    pub run: String,
    pub revision: u64,
    pub port: String,
    pub path: Vec<String>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub analysis: String,
    pub source: Option<SourceIdentity>,
    pub profile: String,
    pub profile_revision: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Reading,
    Processing,
    Finishing,
    Committing,
    Complete,
    Stopped,
    Cancelled,
}
#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    /// Counts include only accepted candidates, not the in-flight invocation.
    pub usage: Usage,
    /// Only acknowledged rejected frames. This is not a row list or a decode-success claim.
    pub coverage: Option<crate::storage::datasets::CoverageProgress>,
    pub committed_position: u64,
    pub read_position: u64,
    pub extent: u64,
    pub framed: bool,
    pub finish_applied: bool,
    pub producer_complete: Option<bool>,
    pub work_allowance: u64,
    pub regex: wes_core::RegexCacheUsage,
}
#[derive(Clone, Debug)]
pub struct Stop {
    pub failure: Failure,
    pub dimension: Option<Dimension>,
    /// Original bytes where framing failed, not calculation source offsets.
    pub source_span: Option<ByteSpan>,
}
pub enum Poll {
    Yield,
    ReadPage,
    ReadHead,
    /// No more source or callback work may enter until the durable grant is acknowledged.
    Grant,
    /// Reconcile measured work at the unchanged committed boundary before a new lease.
    Settle,
    /// A pure candidate is ready; its state/cursor remain at the prior acknowledgement.
    Commit,
    Terminal,
}
/// The runtime integration must preserve failed/cancelled state when `stop` is
/// present. A bounded partial result is inspectable data, not a successful run.
pub struct Completion {
    pub value: Value,
    pub progress: Progress,
    pub stop: Option<Stop>,
    _ledger: Ledger,
}
impl Completion {
    pub fn into_parts(self) -> (Value, Progress, Option<Stop>, crate::driver::ExecutionHold) {
        (
            self.value,
            self.progress,
            self.stop,
            crate::driver::ExecutionHold::retain(self._ledger),
        )
    }
}
struct Invocation {
    machine: Machine,
    range: Option<(u64, u64, u64)>,
    finishing: bool,
}
struct Candidate {
    state: Value,
    state_charge: u64,
    outputs: Vec<Data>,
    coverage: Vec<wes_core::framing::Rejection>,
    output_charge: u64,
    held_after: u64,
    provenance: Provenance,
    range: Option<(u64, u64, u64)>,
    finishing: bool,
    terminal: bool,
    usage: Usage,
    inputs: usize,
    output_ranges: Vec<(u64, u64)>,
    flush: bool,
}
pub struct Runner {
    source: CapturedSource,
    state: Value,
    state_charge: u64,
    context: Value,
    step: Transition,
    finish: Option<Transition>,
    settings: Settings,
    ledger: Ledger,
    base_charge: u64,
    outputs: Vec<Data>,
    coverage: Option<crate::storage::datasets::CoverageProgress>,
    dataset: Option<wes_core::DatasetRef>,
    writer_lease: Option<crate::storage::datasets::DatasetWriteLease>,
    candidate: Option<Candidate>,
    committed_position: u64,
    invocation: Option<Invocation>,
    pending_source: Option<SourcePoll>,
    renewal: bool,
    read_work_demand: u64,
    cache: Option<IterRegexCache>,
    regex: wes_core::RegexCacheUsage,
    provenance: Provenance,
    result_contract: Arc<Contract>,
    result_metadata: ValueMetadata,
    output_metadata: ValueMetadata,
    output_shell_charge: u64,
    services: Option<Arc<dyn LocalServices>>,
    token: CancellationToken,
    started: Instant,
    elapsed_prior: Duration,
    span: Span,
    phase: Phase,
    finish_applied: bool,
    stop: Option<Stop>,
    identity: Identity,
    live: bool,
    durable: Option<durable::Durable>,
}
mod batch;
mod continuation;
mod durable;
pub use continuation::{ContinuationReason, ContinuationReview};
mod forensic;
mod frozen;
mod scheduling;
pub use durable::PreparedResume;
use frozen::FrozenSettings;
impl Runner {
    fn write_owner(&self) -> crate::storage::datasets::DatasetWriteOwner {
        crate::storage::datasets::DatasetWriteOwner {
            role: crate::storage::datasets::DatasetWriteRole::Analysis,
            lineage: self.identity.analysis.clone(),
            run: self.durable.as_ref().map_or_else(
                || self.identity.analysis.clone(),
                |d| d.checkpoint.run.clone(),
            ),
        }
    }
    pub(crate) fn retain_writer(&mut self, lease: crate::storage::datasets::DatasetWriteLease) {
        self.writer_lease = Some(lease);
    }
    pub fn dataset_admission(&self) -> Result<crate::storage::datasets::DatasetCreate, Failure> {
        use crate::storage::datasets::{DatasetCreate, DatasetKind};
        if self.provenance.policy().is_private() || self.provenance.policy().is_unknown() {
            return Err(Failure::new(
                "CAL004",
                self.span,
                "durable scan refuses private or unknown-policy input; choose sink:memory",
            ));
        }
        let schema = wes_core::contracts::ResolvedContractBundle::capture(
            self.step.output_contract.clone(),
            Default::default(),
        )
        .map_err(|_| {
            Failure::new(
                "CAL006",
                self.span,
                "scan output schema exceeds its capture budget",
            )
        })?;
        Ok(DatasetCreate {
            coverage: self.coverage.as_ref().map(|c| c.policy),
            owner: Some(self.write_owner()),
            checkpoint: None,
            recording: None,
            dataset: uuid::Uuid::new_v4().to_string(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: DatasetKind::Analysis,
            schema,
            source: self.source_extent(),
            policy: self.provenance.policy().clone(),
        })
    }
    fn source_extent(&self) -> crate::storage::datasets::SourceExtent {
        use crate::storage::datasets::{SourceExtent, SourceUnit};
        SourceExtent {
            identity: self.identity.analysis.clone(),
            unit: if self.source.framed() {
                SourceUnit::Bytes
            } else {
                SourceUnit::Records
            },
            start: 0,
            end: self.source.total(),
        }
    }
    /// Switch the sink only before source processing, after owned-store admission.
    pub fn attach_dataset(&mut self, reference: wes_core::DatasetRef) -> Result<(), Failure> {
        if self.dataset.is_some()
            || self.invocation.is_some()
            || self.candidate.is_some()
            || self.ledger.usage().input_records != 0
            || reference.records() != 0
            || reference.schema_digest() != self.step.output_contract.digest()
            || self.phase != Phase::Reading
        {
            return Err(Failure::new(
                "CAL003",
                self.span,
                "dataset sink does not match the admitted scan",
            ));
        }
        self.result_contract = result_contract_for_sink(&self.step, true, self.span)?;
        self.result_metadata = ValueMetadata::capture(&self.result_contract);
        self.dataset = Some(reference);
        Ok(())
    }
    pub fn dataset_candidate(&self) -> Result<crate::storage::datasets::DatasetAppend, Failure> {
        use crate::storage::datasets::{DatasetAppend, DatasetLifecycle, DatasetRow};
        let candidate = self
            .candidate
            .as_ref()
            .ok_or_else(|| Failure::new("CAL003", self.span, "scan has no pending candidate"))?;
        let previous = self
            .dataset
            .clone()
            .ok_or_else(|| Failure::new("CAL003", self.span, "scan has no dataset sink"))?;
        let metadata = Some(self.output_metadata.clone());
        let mut rows = Vec::with_capacity(candidate.outputs.len());
        for (offset, data) in candidate.outputs.iter().enumerate() {
            let value = Value::new(
                self.step.output.clone(),
                data.clone(),
                candidate.provenance.clone(),
            )
            .map_err(|_| {
                Failure::new(
                    "CAL002",
                    self.span,
                    "validated scan output lost its captured shape",
                )
            })?
            .with_metadata(metadata.clone());
            rows.push(DatasetRow {
                ordinal: previous
                    .records()
                    .checked_add(offset as u64)
                    .ok_or_else(|| {
                        Failure::new("CAL006", self.span, "scan output ordinal overflow")
                    })?,
                source_start: candidate.output_ranges[offset].0,
                source_end: candidate.output_ranges[offset].1,
                value,
            });
        }
        Ok(DatasetAppend {
            coverage: candidate.coverage.clone(),
            owner: Some(self.write_owner()),
            checkpoint: self.candidate_checkpoint()?,
            recording: None,
            previous,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows,
            source: self.source_extent(),
            lifecycle: if candidate.terminal {
                DatasetLifecycle::Sealed
            } else {
                DatasetLifecycle::Open
            },
            policy: candidate.provenance.policy().clone(),
        })
    }
    /// The outer executor joins the store operation before calling this acknowledgement.
    pub fn acknowledge_dataset(&mut self, reference: wes_core::DatasetRef) -> Result<(), Failure> {
        let prior = self
            .dataset
            .as_ref()
            .ok_or_else(|| Failure::new("CAL003", self.span, "scan has no dataset sink"))?;
        let candidate = self
            .candidate
            .as_ref()
            .ok_or_else(|| Failure::new("CAL003", self.span, "scan has no pending candidate"))?;
        if reference.store() != prior.store()
            || reference.dataset() != prior.dataset()
            || reference.schema_digest() != prior.schema_digest()
            || reference.authorization_generation() != prior.authorization_generation()
            || prior.generation().checked_add(1) != Some(reference.generation())
            || prior.records().checked_add(candidate.outputs.len() as u64)
                != Some(reference.records())
        {
            return Err(Failure::new(
                "CAL002",
                self.span,
                "dataset acknowledgement does not match the pending scan candidate",
            ));
        }
        let checkpoint = self.candidate_checkpoint()?;
        let candidate = self.candidate.take().expect("validated candidate");
        if let (Some(durable), Some(checkpoint)) = (&mut self.durable, checkpoint) {
            self.coverage = checkpoint.coverage.clone();
            durable.checkpoint = checkpoint;
            durable.grant_start_work = self.ledger.usage().work;
        }
        self.dataset = Some(reference);
        self.acknowledge_batch(candidate)
            .map_err(|stop| stop.failure)
    }
    pub fn refuse_dataset(&mut self, message: String) {
        if let Some(durable) = &mut self.durable {
            durable.storage_failed = true;
        }
        self.fail(stop(Failure::new("CAL004", self.span, message)));
    }
    pub fn refuse_stop(&mut self, stop: Stop) {
        self.fail(stop);
    }
    pub fn refuse_scan(&mut self, failure: Failure) {
        self.fail(stop(failure));
    }
    pub fn new(
        input: Input,
        settings: Settings,
        pool: &MemoryPool,
        services: Option<Arc<dyn LocalServices>>,
        token: CancellationToken,
        span: Span,
    ) -> Result<Self, Failure> {
        if !settings.valid() {
            return Err(Failure::new(
                "CAL006",
                span,
                "invalid finite scan admission",
            ));
        }
        let ledger = Ledger::with_startup(settings.limits, pool, settings.startup_work)
            .map_err(|error| refusal(error, span).failure)?;
        Self::new_admitted(input, settings, ledger, services, token, span)
    }
    pub(super) fn new_admitted(
        mut input: Input,
        settings: Settings,
        mut ledger: Ledger,
        services: Option<Arc<dyn LocalServices>>,
        token: CancellationToken,
        span: Span,
    ) -> Result<Self, Failure> {
        if input.live
            && (!matches!(input.source.data(), Data::Dataset(_)) || input.framing.is_some())
        {
            return Err(Failure::new(
                "CAL004",
                span,
                "live scan requires TypedRecords from a committed EventLog Dataset",
            ));
        }
        if let Data::Dataset(reference) = input.source.data() {
            input.source = input.source.with_provenance(
                input.source.provenance().clone().with_policy(
                    &input
                        .source
                        .provenance()
                        .policy()
                        .clone()
                        .read_from_dataset(reference),
                ),
            );
        }
        let bounded =
            |s: &str| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control);
        if !bounded(&input.identity.analysis)
            || !bounded(&input.identity.profile)
            || !bounded(&input.identity.profile_revision)
            || input.identity.source.as_ref().is_some_and(|origin| {
                !bounded(&origin.node)
                    || !bounded(&origin.run)
                    || !bounded(&origin.port)
                    || origin.revision > i64::MAX as u64
                    || origin.path.len() > 64
                    || origin.path.iter().any(|s| s.len() > 1024)
            })
        {
            return Err(Failure::new(
                "CAL004",
                span,
                "invalid captured scan identity",
            ));
        }
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let source_charge = charge(&input.source, settings.source_charge, span)?;
        let state_charge = charge(&input.initial, settings.state_charge, span)?;
        let context_charge = charge(&input.context, settings.context_charge, span)?;
        if input.step.finish {
            return Err(Failure::new(
                "CAL009",
                span,
                "scan step cannot be a finish definition",
            ));
        }
        if let Some(finish) = &input.finish {
            if !finish.finish
                || finish.state.digest() != input.step.state.digest()
                || finish.context.digest() != input.step.context.digest()
                || finish.output_contract.digest() != input.step.output_contract.digest()
            {
                return Err(Failure::new(
                    "CAL009",
                    span,
                    "scan finish must share the captured state, context and output contracts",
                ));
            }
        }
        let framing = match &input.framing {
            Some(profile) => framing_charge(profile)
                .ok_or_else(|| Failure::new("CAL004", span, "invalid scan framing profile"))?,
            None => 0,
        };
        let cache_charge = (settings.patterns as u64)
            .checked_mul(IterRegexCache::COMPILED_CHARGE + 16_384 * 8)
            .ok_or_else(|| Failure::new("CAL006", span, "scan regex reservation overflow"))?;
        // Metadata has its own globally bounded capture, but its retained charge
        // varies with the declaration. Admit both supported sink projections
        // before constructing cursor/VM owners instead of applying an unrelated
        // fixed 128 KiB threshold to a valid captured output contract.
        let output_metadata = ValueMetadata::capture(&input.step.output_contract);
        let output_shell_charge =
            crate::value_size::shape_charge(&input.step.output, settings.limits.memory_bytes)
                .and_then(|n| n.checked_add(output_metadata.charge()))
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| {
                    Failure::new(
                        "CAL006",
                        span,
                        "scan output declaration exceeds its held budget",
                    )
                })?;
        let result_contract = result_contract(&input.step, span)?;
        let result_metadata = ValueMetadata::capture(&result_contract);
        let dataset_contract = result_contract_for_sink(&input.step, true, span)?;
        let dataset_metadata = ValueMetadata::capture(&dataset_contract);
        let declaration_charge = [
            (&result_contract, &result_metadata),
            (&dataset_contract, &dataset_metadata),
        ]
        .into_iter()
        .try_fold(0u64, |maximum, (contract, metadata)| {
            let shape =
                crate::value_size::shape_charge(&contract.shape(), settings.limits.memory_bytes)
                    .ok_or_else(|| {
                        Failure::new(
                            "CAL006",
                            span,
                            "scan result declaration exceeds its held budget",
                        )
                    })?;
            let charge = sum(&[metadata.charge(), shape], span)?
                .checked_mul(2)
                .ok_or_else(|| Failure::new("CAL006", span, "scan declaration charge overflow"))?;
            Ok::<_, Failure>(maximum.max(charge))
        })?;
        // Native schema wrappers/metadata and terminal receipt have a bounded
        // reservation of their own, including the copy at the final handoff.
        let identity_charge = identity_charge(&input.identity, span)?;
        let base_charge = sum(
            &[
                source_charge,
                if matches!(input.source.data(), Data::Dataset(_)) {
                    source_charge
                } else {
                    0
                },
                if matches!(input.source.data(), Data::Dataset(_)) {
                    settings.record_charge
                } else {
                    0
                },
                context_charge,
                identity_charge,
                input.step.code_charge,
                input.finish.as_ref().map_or(0, |f| f.code_charge),
                framing,
                cache_charge,
                declaration_charge,
                output_shell_charge,
                256 * 1024,
            ],
            span,
        )?;
        let peak = sum(
            &[
                base_charge,
                state_charge,
                settings.record_charge,
                settings.scratch.bytes.checked_mul(3).ok_or_else(|| {
                    Failure::new("CAL006", span, "scan scratch reservation overflow")
                })?,
            ],
            span,
        )?;
        ledger
            .held(peak)
            .map_err(|error| refusal(error, span).failure)?;
        ledger
            .work(
                source_charge / 16
                    + state_charge
                        .saturating_add(context_charge)
                        .saturating_add(1024),
            )
            .map_err(|error| refusal(error, span).failure)?;
        let provenance = Provenance::agreed_by(
            [
                input.source.provenance(),
                input.initial.provenance(),
                input.context.provenance(),
            ]
            .into_iter()
            .chain(input.control.as_ref()),
        );
        let state = input
            .step
            .argument("state", &input.initial, &|| token.is_cancelled(), span)?;
        let context =
            input
                .step
                .argument("context", &input.context, &|| token.is_cancelled(), span)?;
        let coverage = input
            .framing
            .as_ref()
            .and_then(|profile| match profile.malformed {
                wes_core::framing::Malformed::Strict {} => None,
                wes_core::framing::Malformed::Forensic { excerpt_bytes } => {
                    Some(crate::storage::datasets::CoverageProgress::empty(
                        crate::storage::datasets::CoveragePolicy {
                            excerpt_bytes: excerpt_bytes as u32,
                        },
                    ))
                }
            });
        let source = CapturedSource::new(
            input.source,
            input.framing,
            settings.source_charge,
            settings.record_charge,
            span,
        )?;
        let item_shape = source.item_shape();
        if item_shape != wes_core::Shape::Unknown
            && !item_shape.is_assignable_to(&input.step.input.shape())
        {
            return Err(Failure::new(
                "CAL017",
                span,
                "captured scan source item shape does not satisfy its transition input",
            ));
        }
        if let Some(finish) = &input.finish {
            finish.argument(
                "end",
                &source_end(
                    source.total(),
                    source.framed(),
                    provenance.clone(),
                    input.live,
                ),
                &|| token.is_cancelled(),
                span,
            )?;
        }
        let cache = IterRegexCache::with_capacity(settings.patterns)
            .map_err(|error| Failure::iteration(error, span))?;
        ledger
            .held(sum(&[base_charge, state_charge], span)?)
            .map_err(|error| refusal(error, span).failure)?;
        Ok(Self {
            source,
            state,
            state_charge,
            context,
            step: input.step,
            finish: input.finish,
            settings,
            ledger,
            base_charge,
            outputs: vec![],
            coverage,
            dataset: None,
            writer_lease: None,
            candidate: None,
            committed_position: 0,
            invocation: None,
            pending_source: None,
            renewal: false,
            read_work_demand: 0,
            cache: Some(cache),
            regex: Default::default(),
            provenance,
            result_contract,
            result_metadata,
            output_metadata,
            output_shell_charge,
            services,
            token,
            started: Instant::now(),
            elapsed_prior: Duration::ZERO,
            span,
            phase: Phase::Reading,
            finish_applied: false,
            stop: None,
            identity: input.identity,
            live: input.live,
            durable: None,
        })
    }
    pub fn progress(&self) -> Progress {
        Progress {
            phase: self.phase,
            usage: self.ledger.usage(),
            coverage: self.coverage.clone(),
            committed_position: self.committed_position,
            read_position: self.source.position(),
            extent: self.source.total(),
            framed: self.source.framed(),
            finish_applied: self.finish_applied,
            producer_complete: self.source.producer_status(),
            work_allowance: self.ledger.work_allowance(),
            regex: self
                .invocation
                .as_ref()
                .map_or(self.regex, |active| active.machine.usage().regex),
        }
    }
    pub fn execution_progress(&self) -> crate::driver::progress::ExecutionProgress {
        use crate::driver::progress::{Counters, ExecutionProgress, Phase as P, PositionUnit};
        let progress = self.progress();
        let usage = progress.usage;
        ExecutionProgress::records(
            match progress.phase {
                Phase::Reading => P::Reading,
                Phase::Processing => P::Processing,
                Phase::Finishing => P::Finishing,
                Phase::Committing => P::Committing,
                Phase::Complete => P::Complete,
                Phase::Stopped => P::Stopped,
                Phase::Cancelled => P::Cancelled,
            },
            Some(Counters {
                committed_position: progress.committed_position,
                read_position: progress.read_position,
                extent: progress.extent,
                unit: if progress.framed {
                    PositionUnit::Bytes
                } else {
                    PositionUnit::Records
                },
                input_records: usage.input_records,
                output_records: usage.output_records,
                work: usage.work,
                work_allowance: progress.work_allowance,
                work_limit: self.settings.limits.work,
                held_charge: usage.held_bytes,
                high_water_charge: usage.high_water_bytes,
                held_limit: self.settings.limits.memory_bytes,
                output_charge: usage.output_bytes,
                output_limit: self.settings.limits.output_bytes,
            }),
        )
        .restricted(self.provenance.policy())
    }
    /// One bounded native read or one VM quantum; never a loop over all records.
    pub fn poll(&mut self) -> Poll {
        if matches!(
            self.phase,
            Phase::Complete | Phase::Stopped | Phase::Cancelled
        ) {
            return Poll::Terminal;
        }
        if self.token.is_cancelled() {
            return self.fail(Stop {
                failure: Failure::cancelled(self.span),
                dimension: None,
                source_span: None,
            });
        }
        if self.live && self.source.followed_source().is_none() {
            return self.fail(stop(Failure::new(
                "CAL004",
                self.span,
                "live source has not been admitted by its owned store",
            )));
        }
        if self.coverage.is_some() && self.durable.is_none() {
            return self.fail(stop(Failure::new(
                "CAL004",
                self.span,
                "forensic framing requires an acknowledged durable Dataset sink",
            )));
        }
        if self.candidate_ready() {
            self.phase = Phase::Committing;
            return Poll::Commit;
        }
        if self.elapsed() >= self.settings.duration {
            return self.fail(Stop {
                failure: Failure::new("CAL006", self.span, "scan duration limit reached"),
                dimension: Some(Dimension::Duration),
                source_span: None,
            });
        }
        if self.needs_durable_grant() {
            self.phase = Phase::Committing;
            return Poll::Grant;
        }
        if self.renewal {
            self.phase = Phase::Committing;
            return Poll::Settle;
        }
        match self.poll_inner() {
            Ok(poll) => poll,
            Err(stop) => self.fail(stop),
        }
    }
    fn fail(&mut self, mut stop: Stop) -> Poll {
        if let Some(candidate) = &self.candidate {
            stop.failure.policy = stop.failure.policy.join(candidate.provenance.policy());
        }
        stop.failure.policy = stop.failure.policy.join(self.provenance.policy());
        self.provenance = self.provenance.clone().with_policy(&stop.failure.policy);
        self.phase = if stop.failure.cancelled {
            Phase::Cancelled
        } else {
            Phase::Stopped
        };
        self.invocation = None;
        self.pending_source = None;
        self.renewal = false;
        self.candidate = None;
        self.stop = Some(stop);
        // No finish after failure, no callback retry, no partial candidate commit.
        let held = self
            .base_charge
            .saturating_add(self.state_charge)
            .saturating_add(self.retained_output_charge());
        let _ = self.ledger.held(held);
        Poll::Terminal
    }
    fn poll_inner(&mut self) -> Result<Poll, Stop> {
        if let Some(mut active) = self.invocation.take() {
            match active.machine.poll(&self.token).map_err(stop)? {
                calc::Step::Yield => {
                    self.invocation = Some(active);
                    return Ok(Poll::Yield);
                }
                calc::Step::Request(request) => {
                    if self.elapsed() >= self.settings.duration {
                        return Err(Stop {
                            failure: Failure::new(
                                "CAL006",
                                self.span,
                                "scan duration reached before entering a native service",
                            ),
                            dimension: Some(Dimension::Duration),
                            source_span: None,
                        });
                    }
                    let id = request.id();
                    let result = match request {
                        Request::Call { span, .. } => {
                            return Err(stop(Failure::new(
                                "CAL002",
                                span,
                                "pure scan requested external execution; no provider was entered",
                            )));
                        }
                        Request::Json {
                            bytes,
                            mode,
                            contract,
                            span,
                            ..
                        } => {
                            active
                                .machine
                                .charge_native_work(
                                    (bytes.len() as u64).saturating_mul(16).saturating_add(1024),
                                    span,
                                )
                                .map_err(stop)?;
                            let services = self.services.as_ref().ok_or_else(|| {
                                stop(Failure::new(
                                    "CAL004",
                                    span,
                                    "scan JSON local service is unavailable",
                                ))
                            })?;
                            services.json(&bytes, contract.as_deref(), &self.token, span, mode)
                        }
                        Request::Http {
                            operation,
                            input,
                            span,
                            ..
                        } => {
                            let charge =
                                charge(&input, self.settings.record_charge, span).map_err(stop)?;
                            active
                                .machine
                                .charge_native_work(charge.saturating_add(1024), span)
                                .map_err(stop)?;
                            self.services
                                .as_ref()
                                .ok_or_else(|| {
                                    stop(Failure::new(
                                        "CAL004",
                                        span,
                                        "scan HTTP local service is unavailable",
                                    ))
                                })?
                                .http(operation, &input, &self.token, span)
                        }
                    };
                    active.machine.resume(id, result).map_err(stop)?;
                    self.invocation = Some(active);
                    return Ok(Poll::Yield);
                }
                calc::Step::Complete(value) => {
                    self.accept(value, active.range, active.finishing)?;
                    self.regex = active.machine.usage().regex;
                    self.cache = Some(active.machine.into_regex_cache());
                    self.phase = if active.finishing {
                        Phase::Complete
                    } else {
                        Phase::Reading
                    };
                    if self.candidate_ready() {
                        self.phase = Phase::Committing;
                        return Ok(Poll::Commit);
                    }
                    return Ok(if active.finishing {
                        Poll::Terminal
                    } else {
                        Poll::Yield
                    });
                }
            }
        }
        // Covers native cursor buffers and conversion before source.poll allocates.
        self.ledger
            .held(
                sum(
                    &[
                        self.base_charge,
                        self.held_state_charge(),
                        self.retained_output_charge(),
                        self.settings.record_charge,
                    ],
                    self.span,
                )
                .map_err(stop)?,
            )
            .map_err(|error| refusal(error, self.span))?;
        let polled =
            if let Some(pending) = self.pending_source.take() {
                pending
            } else {
                let can_flush = self.candidate.is_some();
                let ledger = &mut self.ledger;
                self.source
                    .poll_scheduled(self.settings.block, &self.token, self.span, |amount| {
                        match ledger.scheduled_work(amount) {
                            Err(Refusal {
                                dimension: Dimension::WorkAllowance,
                                ..
                            }) if can_flush => Ok(false),
                            result => result,
                        }
                    })
                    .map_err(|error| Stop {
                        failure: error.failure,
                        dimension: error.dimension,
                        source_span: error.source_span,
                    })?
            };
        if self.needs_handoff_grant(&polled)? {
            self.pending_source = Some(polled);
            return self.request_renewal();
        }
        match polled {
            SourcePoll::Renew => self.request_renewal(),
            SourcePoll::Pending => Ok(Poll::Yield),
            SourcePoll::ReadPage => {
                if let Some(candidate) = &mut self.candidate {
                    candidate.flush = true;
                    self.phase = Phase::Committing;
                    Ok(Poll::Commit)
                } else {
                    Ok(Poll::ReadPage)
                }
            }
            SourcePoll::ReadHead => {
                if let Some(candidate) = &mut self.candidate {
                    candidate.flush = true;
                    self.phase = Phase::Committing;
                    Ok(Poll::Commit)
                } else {
                    Ok(Poll::ReadHead)
                }
            }
            SourcePoll::Record {
                ordinal,
                value,
                start,
                end,
                input_charge,
            } => {
                self.validate_source_boundary(ordinal, start, end)?;
                self.ledger
                    .admit_input_from(self.working_usage(), input_charge)
                    .map_err(|error| refusal(error, self.span))?;
                self.begin(value, Some((start, end, input_charge)), false)?;
                Ok(Poll::Yield)
            }
            SourcePoll::Rejected(row) => {
                self.accept_rejection(row)?;
                if self.candidate_ready() {
                    self.phase = Phase::Committing;
                    Ok(Poll::Commit)
                } else {
                    Ok(Poll::Yield)
                }
            }
            SourcePoll::Incomplete => {
                if let Some(candidate) = &mut self.candidate {
                    candidate.flush = true;
                    self.phase = Phase::Committing;
                    return Ok(Poll::Commit);
                }
                Err(stop(Failure::new(
                    "CAL004",
                    self.span,
                    "EventLog ended without confirmed natural EOF; committed partial results remain available and finish was not called",
                )))
            }
            SourcePoll::End => {
                if self.finish.is_some() {
                    let end = source_end(
                        self.source.position(),
                        self.source.framed(),
                        self.working_provenance().clone(),
                        self.source.producer_complete(),
                    );
                    self.begin(end, None, true)?;
                    Ok(Poll::Yield)
                } else {
                    if self.dataset.is_some() {
                        self.stage_batch(Candidate {
                            state: self.working_state().clone(),
                            state_charge: self.working_state_charge(),
                            outputs: vec![],
                            coverage: vec![],
                            output_charge: 0,
                            held_after: self.base_charge.saturating_add(self.state_charge),
                            provenance: self.working_provenance().clone(),
                            range: None,
                            finishing: false,
                            terminal: true,
                            usage: self.working_usage(),
                            inputs: 0,
                            output_ranges: vec![],
                            flush: true,
                        })?;
                        self.phase = Phase::Committing;
                        return Ok(Poll::Commit);
                    }
                    self.phase = Phase::Complete;
                    self.ledger
                        .held(
                            sum(
                                &[
                                    self.base_charge,
                                    self.working_state_charge(),
                                    self.ledger.usage().output_bytes,
                                ],
                                self.span,
                            )
                            .map_err(stop)?,
                        )
                        .map_err(|error| refusal(error, self.span))?;
                    Ok(Poll::Terminal)
                }
            }
        }
    }
    fn begin(
        &mut self,
        item: Value,
        range: Option<(u64, u64, u64)>,
        finishing: bool,
    ) -> Result<(), Stop> {
        let transition = if finishing {
            self.finish.as_ref().expect("captured finish")
        } else {
            &self.step
        };
        let amount = charge(&item, self.settings.record_charge, self.span).map_err(stop)?;
        self.ledger
            .work(
                amount
                    .saturating_add(self.working_state_charge())
                    .saturating_add(1024),
            )
            .map_err(|error| refusal(error, self.span))?;
        self.ledger
            .held(
                sum(
                    &[
                        self.base_charge,
                        self.held_state_charge(),
                        self.retained_output_charge(),
                        self.settings.record_charge,
                        self.settings.scratch.bytes * 3,
                    ],
                    self.span,
                )
                .map_err(stop)?,
            )
            .map_err(|error| refusal(error, self.span))?;
        let name = if finishing { "end" } else { "item" };
        let cancelled = || self.token.is_cancelled();
        let inputs = IndexMap::from([
            // These immutable owners already passed the same captured contracts
            // at setup or the previous atomic candidate boundary.
            ("state".into(), self.working_state().clone()),
            ("context".into(), self.context.clone()),
            (
                name.into(),
                transition
                    .argument(name, &item, &cancelled, self.span)
                    .map_err(stop)?,
            ),
        ]);
        let mut machine = Machine::new_record(
            transition.definition.compiled.clone(),
            inputs,
            self.settings.scratch,
            self.token.clone(),
            self.ledger.work_counter(),
            self.cache.take().expect("idle cache owner"),
        )
        .map_err(stop)?;
        // Filtering or zero outputs must not erase policy from the whole source.
        machine.capture_control_provenance(self.provenance.clone());
        self.phase = if finishing {
            Phase::Finishing
        } else {
            Phase::Processing
        };
        self.invocation = Some(Invocation {
            machine,
            range,
            finishing,
        });
        Ok(())
    }
    pub fn source_page_request(
        &self,
    ) -> Result<(wes_core::DatasetRef, crate::storage::datasets::PageRequest), Failure> {
        let reference =
            self.source.dataset().cloned().ok_or_else(|| {
                Failure::new("CAL004", self.span, "analysis has no dataset source")
            })?;
        Ok((
            reference,
            crate::storage::datasets::PageRequest {
                charge: Some(self.source.page_charge(self.span)?),
                from: self.source.ordinal(),
                rows: self.settings.page_rows,
                bytes: self.settings.page_bytes,
                segments: self.settings.page_segments,
                work: Some(crate::storage::datasets::ReadWork::new(
                    self.ledger.work_counter(),
                )),
            },
        ))
    }
    pub fn acknowledge_source_page(
        &mut self,
        page: crate::storage::datasets::DatasetPage,
    ) -> Result<(), Failure> {
        self.source.acknowledge_page(page, self.span)?;
        self.read_work_demand = 0;
        Ok(())
    }
    pub(crate) fn follow_source(
        &mut self,
        info: crate::storage::datasets::DatasetInfo,
    ) -> Result<(), Failure> {
        let coverage = info.recording.ok_or_else(|| {
            Failure::new(
                "CAL004",
                self.span,
                "live scan requires a recorded EventLog, not an analysis Dataset",
            )
        })?;
        if !self.live || info.schema.root().shape() != self.source.item_shape() {
            return Err(Failure::new(
                "CAL004",
                self.span,
                "live source schema does not match its captured input",
            ));
        }
        self.provenance = self.provenance.clone().with_policy(
            &self
                .provenance
                .policy()
                .join(&info.policy)
                .read_from_dataset(&info.reference),
        );
        self.source.follow(
            crate::storage::datasets::FollowedSource {
                prefix: info.reference,
                run: coverage.run,
                epoch: coverage.epoch,
                first: coverage.first,
            },
            self.span,
        )
    }
    pub(crate) fn followed_dataset(&self) -> Option<&str> {
        self.source
            .followed_source()
            .map(|source| source.prefix.dataset())
    }
    pub(crate) fn source_head_request(
        &mut self,
    ) -> Result<(wes_core::DatasetRef, crate::storage::datasets::ReadWork), Failure> {
        Ok((
            self.source.dataset().cloned().ok_or_else(|| {
                Failure::new("CAL004", self.span, "analysis has no recorded source")
            })?,
            crate::storage::datasets::ReadWork::new(self.ledger.work_counter()),
        ))
    }
    pub(crate) fn acknowledge_source_head(
        &mut self,
        info: crate::storage::datasets::DatasetInfo,
    ) -> Result<bool, Failure> {
        self.provenance = self.provenance.clone().with_policy(
            &self
                .provenance
                .policy()
                .join(&info.policy)
                .read_from_dataset(&info.reference),
        );
        let advanced = self.source.acknowledge_head(info, self.span)?;
        self.read_work_demand = 0;
        Ok(advanced)
    }
    fn elapsed(&self) -> Duration {
        self.elapsed_prior.saturating_add(self.started.elapsed())
    }
    fn elapsed_ms(&self) -> u64 {
        self.elapsed()
            .as_millis()
            .min(self.settings.duration.as_millis()) as u64
    }
    pub(crate) fn remaining_duration(&self) -> Duration {
        self.settings.duration.saturating_sub(self.elapsed())
    }
    fn accept(
        &mut self,
        value: Value,
        range: Option<(u64, u64, u64)>,
        finishing: bool,
    ) -> Result<(), Stop> {
        // All callback ownership still exists here. Admit before validation and
        // conversion; its whole arena is dropped only after a candidate handoff.
        let candidate_charge =
            charge(&value, self.settings.scratch.bytes, self.span).map_err(stop)?;
        self.ledger
            .work(candidate_charge.saturating_add(1024))
            .map_err(|error| refusal(error, self.span))?;
        let transition = if finishing {
            self.finish.as_ref().expect("captured finish")
        } else {
            &self.step
        };
        let checked = transition
            .result(&value, &|| self.token.is_cancelled(), self.span)
            .map_err(stop)?;
        let provenance = self.provenance.merge(checked.provenance());
        let metadata = checked.metadata().and_then(|meta| meta.project("/f:state"));
        let Data::Record(mut fields) = checked.into_data() else {
            unreachable!("checked step result");
        };
        let state_data = fields.shift_remove("state").expect("checked state");
        let Data::List(outputs) = fields.shift_remove("outputs").expect("checked outputs") else {
            unreachable!("checked output list");
        };
        let state = Value::new(transition.state.shape(), state_data, provenance.clone())
            .map_err(|_| {
                stop(Failure::new(
                    "CAL017",
                    self.span,
                    "scan candidate state does not match its declared shape",
                ))
            })?
            .with_metadata(metadata);
        let state_charge = charge(&state, self.settings.state_charge, self.span).map_err(stop)?;
        if outputs.len() as u64 > self.settings.outputs_per_record {
            return Err(refusal(
                Refusal {
                    dimension: Dimension::RecordOutputs,
                    limit: self.settings.outputs_per_record,
                },
                self.span,
            ));
        }
        let output_charge = outputs.iter().try_fold(0u64, |total, item| {
            let amount = crate::value_size::data_charge(item, self.settings.scratch.bytes)
                .ok_or_else(|| {
                    stop(Failure::new(
                        "CAL006",
                        self.span,
                        "scan candidate output exceeds its charge limit",
                    ))
                })?;
            total.checked_add(amount).ok_or_else(|| {
                stop(Failure::new(
                    "CAL006",
                    self.span,
                    "scan output charge overflow",
                ))
            })
        })?;
        if self.token.is_cancelled() {
            return Err(stop(Failure::cancelled(self.span)));
        }
        if self.elapsed() >= self.settings.duration {
            return Err(Stop {
                failure: Failure::new(
                    "CAL006",
                    self.span,
                    "scan duration limit reached before candidate commit",
                ),
                dimension: Some(Dimension::Duration),
                source_span: None,
            });
        }
        let held_after = sum(
            &[
                self.base_charge,
                state_charge,
                self.retained_output_charge(),
                output_charge,
                if self.dataset.is_some() {
                    self.state_charge.saturating_add(
                        self.output_shell_charge
                            .saturating_mul(outputs.len() as u64),
                    )
                } else {
                    0
                },
            ],
            self.span,
        )
        .map_err(stop)?;
        // Reserve possible Vec geometric capacity before appending. Record Data
        // charge includes container overhead; no per-record graph/history nodes.
        let usage = self
            .ledger
            .candidate_usage_from(
                self.working_usage(),
                range.map(|(_, _, charge)| charge),
                outputs.len() as u64,
                output_charge,
                held_after,
            )
            .map_err(|error| refusal(error, self.span))?;
        let output_ranges = vec![
            range.map_or(
                // Finish is admitted only at EOF; preceding inputs may still share this batch.
                (self.source.position(), self.source.position()),
                |(start, end, _)| (start, end)
            );
            outputs.len()
        ];
        let candidate = Candidate {
            state,
            state_charge,
            outputs,
            coverage: vec![],
            output_charge,
            held_after,
            provenance,
            range,
            finishing,
            terminal: finishing,
            usage,
            inputs: usize::from(range.is_some()),
            output_ranges,
            flush: finishing,
        };
        if self.dataset.is_some() {
            self.stage_batch(candidate)?;
        } else {
            self.advance(candidate)?;
        }
        Ok(())
    }
    fn retained_output_charge(&self) -> u64 {
        if self.dataset.is_some() {
            self.candidate.as_ref().map_or(0, |c| {
                c.output_charge.saturating_add(
                    self.output_shell_charge
                        .saturating_mul(c.outputs.len() as u64),
                )
            })
        } else {
            self.ledger.usage().output_bytes
        }
    }
    fn advance(&mut self, candidate: Candidate) -> Result<(), Stop> {
        self.ledger
            .commit(
                candidate.range.map(|(_, _, charge)| charge),
                candidate.outputs.len() as u64,
                candidate.output_charge,
                candidate.held_after,
            )
            .map_err(|error| refusal(error, self.span))?;
        self.state = candidate.state;
        self.state_charge = candidate.state_charge;
        if self.dataset.is_none() {
            self.outputs.extend(candidate.outputs);
        }
        self.provenance = candidate.provenance;
        if let Some((_, end, charge)) = candidate.range {
            self.committed_position = end;
            self.ledger
                .earn(charge.saturating_mul(self.settings.work_per_input_unit));
        }
        self.finish_applied = candidate.finishing;
        self.phase = if candidate.terminal {
            Phase::Complete
        } else {
            Phase::Reading
        };
        Ok(())
    }
    pub(crate) fn current_prefix(&self) -> Result<Value, Failure> {
        let reference = self.dataset.clone().ok_or_else(|| {
            Failure::new(
                "CAL004",
                self.span,
                "live analysis has no acknowledged output prefix",
            )
        })?;
        Value::new(
            self.result_contract.shape(),
            Data::Record(
                [
                    ("state".into(), self.state.data().clone()),
                    ("outputs".into(), Data::Dataset(reference.into())),
                    ("receipt".into(), self.result_receipt()),
                ]
                .into(),
            ),
            self.provenance.clone(),
        )
        .map_err(|_| {
            Failure::new(
                "CAL002",
                self.span,
                "analysis prefix violated its captured result shape",
            )
        })
        .map(|value| value.with_metadata(Some(self.result_metadata.clone())))
    }
    fn result_receipt(&self) -> Data {
        let mut receipt = receipt(&self.progress(), self.stop.as_ref());
        let Data::Record(fields) = &mut receipt else {
            unreachable!()
        };
        let optional = |value: Option<Data>| Data::Option(value.map(Box::new));
        fields.insert(
            "analysisId".into(),
            Data::Text(self.identity.analysis.as_str().into()),
        );
        fields.insert(
            "transitionRevision".into(),
            Data::Text(self.step.revision().into()),
        );
        fields.insert(
            "finishRevision".into(),
            optional(
                self.finish
                    .as_ref()
                    .map(|f| Data::Text(f.revision().into())),
            ),
        );
        fields.insert(
            "profile".into(),
            Data::Text(self.identity.profile.as_str().into()),
        );
        fields.insert(
            "profileRevision".into(),
            Data::Text(self.identity.profile_revision.as_str().into()),
        );
        fields.insert(
            "sourceNode".into(),
            optional(
                self.identity
                    .source
                    .as_ref()
                    .map(|s| Data::Text(s.node.as_str().into())),
            ),
        );
        fields.insert(
            "sourceRun".into(),
            optional(
                self.identity
                    .source
                    .as_ref()
                    .map(|s| Data::Text(s.run.as_str().into())),
            ),
        );
        fields.insert(
            "sourceRevision".into(),
            optional(
                self.identity
                    .source
                    .as_ref()
                    .map(|s| Data::Int(s.revision as i64)),
            ),
        );
        fields.insert(
            "sourcePort".into(),
            optional(
                self.identity
                    .source
                    .as_ref()
                    .map(|s| Data::Text(s.port.as_str().into())),
            ),
        );
        fields.insert(
            "sourcePath".into(),
            Data::List(self.identity.source.as_ref().map_or_else(Vec::new, |s| {
                s.path
                    .iter()
                    .map(|p| Data::Text(p.as_str().into()))
                    .collect()
            })),
        );
        fields.insert("durableResume".into(), Data::Bool(self.resume_available()));
        let durable = self.durable.as_ref();
        let measured = durable.map_or(self.ledger.usage().work, |d| {
            d.checkpoint
                .work
                .completed
                .checked_add(self.ledger.usage().work.saturating_sub(d.grant_start_work))
                .expect("measured work is bounded by the admitted cumulative ledger")
        });
        for (name, value) in [
            (
                "attempt",
                optional(durable.map(|d| Data::Text(d.checkpoint.attempt.clone().into()))),
            ),
            (
                "previousAttempt",
                optional(
                    durable
                        .and_then(|d| d.checkpoint.previous_attempt.clone())
                        .map(|s| Data::Text(s.into())),
                ),
            ),
            (
                "budgetDigest",
                optional(durable.map(|d| Data::Text(d.checkpoint.budget.digest().into()))),
            ),
            (
                "budgetIssuedAttempt",
                optional(
                    durable.map(|d| Data::Text(d.checkpoint.budget.issued_attempt.clone().into())),
                ),
            ),
            (
                "budgetPrevious",
                optional(
                    durable
                        .and_then(|d| d.checkpoint.budget.previous.clone())
                        .map(|s| Data::Text(s.into())),
                ),
            ),
            (
                "authorizedWork",
                Data::Int(durable.map_or(0, |d| d.checkpoint.budget.authorized_work) as i64),
            ),
            (
                "workGrant",
                Data::Int(durable.map_or(0, |d| d.checkpoint.budget.work_grant) as i64),
            ),
            (
                "durationOverrunMs",
                optional(Some(Data::Int(
                    self.elapsed()
                        .saturating_sub(self.settings.duration)
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                ))),
            ),
            ("measuredWork", Data::Int(measured as i64)),
            (
                "outstandingWork",
                optional(durable.map(|d| Data::Int(d.checkpoint.work.outstanding as i64))),
            ),
            ("durationChargedMs", Data::Int(self.elapsed_ms() as i64)),
            (
                "durationOutstandingMs",
                optional(durable.map(|d| Data::Int(d.checkpoint.duration.outstanding_ms as i64))),
            ),
        ] {
            fields.insert(name.into(), value);
        }
        let limits = self.settings.limits;
        fields.insert(
            "limits".into(),
            Data::Record(
                [
                    ("work".into(), Data::Int(limits.work as i64)),
                    ("inputCharge".into(), Data::Int(limits.input_bytes as i64)),
                    (
                        "inputRecords".into(),
                        Data::Int(limits.input_records as i64),
                    ),
                    ("heldCharge".into(), Data::Int(limits.memory_bytes as i64)),
                    ("outputCharge".into(), Data::Int(limits.output_bytes as i64)),
                    (
                        "outputRecords".into(),
                        Data::Int(limits.output_records as i64),
                    ),
                    (
                        "recordWork".into(),
                        Data::Int(self.settings.scratch.work as i64),
                    ),
                    (
                        "recordCharge".into(),
                        Data::Int(self.settings.record_charge as i64),
                    ),
                    (
                        "pageBytes".into(),
                        Data::Int(self.settings.page_bytes as i64),
                    ),
                    (
                        "stateCharge".into(),
                        Data::Int(self.settings.state_charge as i64),
                    ),
                    (
                        "contextCharge".into(),
                        Data::Int(self.settings.context_charge as i64),
                    ),
                    (
                        "durationMs".into(),
                        Data::Int(self.settings.duration.as_millis() as i64),
                    ),
                ]
                .into(),
            ),
        );
        receipt
    }
    /// Availability is execution-owner evidence, not a client guess from one remaining counter.
    fn resume_available(&self) -> bool {
        let usage = self.ledger.usage();
        let limits = self.settings.limits;
        self.durable.as_ref().is_some_and(|d| !d.storage_failed)
            && matches!(self.phase, Phase::Stopped | Phase::Cancelled)
            && !self.finish_applied
            && self.elapsed() < self.settings.duration
            && self.ledger.remaining_work() > 0
            && usage.work < limits.work
            && usage.input_bytes < limits.input_bytes
            && usage.input_records < limits.input_records
            && usage.output_bytes < limits.output_bytes
            && usage.output_records < limits.output_records
            && self
                .stop
                .as_ref()
                .is_some_and(|stop| stop.failure.cancelled)
    }
    /// Consuming this owner also drops any speculative VM/candidate. The caller
    /// keeps the aggregate lease until it hands the completion to the coordinator.
    pub fn into_completion(self) -> Result<Completion, Failure> {
        if !matches!(
            self.phase,
            Phase::Complete | Phase::Stopped | Phase::Cancelled
        ) {
            return Err(Failure::new(
                "CAL003",
                self.span,
                "scan attempt has not terminated",
            ));
        }
        let progress = self.progress();
        let receipt = self.result_receipt();
        let value = Value::new(
            self.result_contract.shape(),
            Data::Record(
                [
                    ("state".into(), self.state.into_data()),
                    (
                        "outputs".into(),
                        self.dataset.map_or_else(
                            || Data::List(self.outputs),
                            |reference| Data::Dataset(reference.into()),
                        ),
                    ),
                    ("receipt".into(), receipt),
                ]
                .into(),
            ),
            self.provenance,
        )
        .map_err(|_| {
            Failure::new(
                "CAL002",
                self.span,
                "scan result construction violated its captured shape",
            )
        })?
        .with_metadata(Some(self.result_metadata));
        Ok(Completion {
            value,
            progress,
            stop: self.stop,
            _ledger: self.ledger,
        })
    }
}
fn charge(value: &Value, limit: u64, span: Span) -> Result<u64, Failure> {
    crate::value_size::value_charge(value, limit).ok_or_else(|| {
        Failure::new(
            "CAL006",
            span,
            "scan value exceeds its captured charge or structural limit",
        )
    })
}
fn sum(parts: &[u64], span: Span) -> Result<u64, Failure> {
    parts
        .iter()
        .try_fold(0u64, |total, n| total.checked_add(*n))
        .ok_or_else(|| Failure::new("CAL006", span, "scan memory reservation overflow"))
}
pub(super) fn stop(failure: Failure) -> Stop {
    let dimension = failure.budget.map(|kind| match kind {
        calc::BudgetKind::Work => Dimension::RecordWork,
        calc::BudgetKind::CumulativeWork => Dimension::Work,
        calc::BudgetKind::CumulativeAllowance => Dimension::WorkAllowance,
        calc::BudgetKind::Memory => Dimension::RecordMemory,
    });
    Stop {
        failure,
        dimension,
        source_span: None,
    }
}
fn identity_charge(identity: &Identity, span: Span) -> Result<u64, Failure> {
    let mut texts = vec![
        identity.analysis.as_str(),
        identity.profile.as_str(),
        identity.profile_revision.as_str(),
    ];
    if let Some(source) = &identity.source {
        texts.extend([
            source.node.as_str(),
            source.run.as_str(),
            source.port.as_str(),
        ]);
        texts.extend(source.path.iter().map(String::as_str));
    }
    // Owned identity and the terminal receipt may coexist at handoff.
    texts
        .into_iter()
        .try_fold(1024u64, |charge, text| {
            charge.checked_add((text.len() as u64).checked_mul(12)?.checked_add(256)?)
        })
        .ok_or_else(|| Failure::new("CAL006", span, "scan identity charge overflow"))
}
fn refusal(error: Refusal, span: Span) -> Stop {
    Stop {
        failure: Failure::new(
            "CAL006",
            span,
            format!("scan {:?} limit reached ({})", error.dimension, error.limit),
        ),
        dimension: Some(error.dimension),
        source_span: None,
    }
}
fn source_end(
    position: u64,
    framed: bool,
    provenance: Provenance,
    producer_complete: bool,
) -> Value {
    let registry = ContractRegistry::new();
    let contract = native_record(
        &registry,
        "ScanEnd",
        &[
            ("kind", "Text"),
            ("position", "Int"),
            ("unit", "Text"),
            ("producerComplete", "Option<Bool>"),
        ],
        Span::new(0, 0).expect("native span"),
    )
    .expect("native end declaration");
    Value::new(
        contract.shape(),
        Data::Record(
            [
                (
                    "kind".into(),
                    Data::Text(
                        if producer_complete {
                            "natural_end"
                        } else {
                            "selected_range"
                        }
                        .into(),
                    ),
                ),
                ("position".into(), Data::Int(position as i64)),
                (
                    "unit".into(),
                    Data::Text(if framed { "bytes" } else { "records" }.into()),
                ),
                (
                    "producerComplete".into(),
                    Data::Option(producer_complete.then(|| Box::new(Data::Bool(true)))),
                ),
            ]
            .into(),
        ),
        provenance,
    )
    .expect("native finite end")
}
fn native_record(
    registry: &ContractRegistry,
    name: &str,
    fields: &[(&str, &str)],
    span: Span,
) -> Result<Arc<Contract>, Failure> {
    let fields = fields
        .iter()
        .map(|(name, expression)| {
            registry
                .resolve(expression)
                .map(|contract| {
                    (
                        String::from(*name),
                        ContractField {
                            contract,
                            optional: false,
                        },
                    )
                })
                .map_err(|error| Failure::new("CAL002", span, error.to_string()))
        })
        .collect::<Result<IndexMap<_, _>, _>>()?;
    Contract::record(name, fields)
        .map(Arc::new)
        .map_err(|error| Failure::new("CAL002", span, error.to_string()))
}
pub(super) fn result_contract(step: &Transition, span: Span) -> Result<Arc<Contract>, Failure> {
    result_contract_for_sink(step, false, span)
}
pub(super) fn result_contract_for_sink(
    step: &Transition,
    dataset: bool,
    span: Span,
) -> Result<Arc<Contract>, Failure> {
    let registry = ContractRegistry::new();
    let limits = native_record(
        &registry,
        "ScanLimits",
        &[
            ("work", "Int"),
            ("inputCharge", "Int"),
            ("inputRecords", "Int"),
            ("heldCharge", "Int"),
            ("outputCharge", "Int"),
            ("outputRecords", "Int"),
            ("recordWork", "Int"),
            ("recordCharge", "Int"),
            ("pageBytes", "Int"),
            ("stateCharge", "Int"),
            ("contextCharge", "Int"),
            ("durationMs", "Int"),
        ],
        span,
    )?;
    let receipt = native_record(
        &registry,
        "ScanReceipt",
        &[
            ("status", "Text"),
            ("position", "Int"),
            ("readPosition", "Int"),
            ("extent", "Int"),
            ("positionUnit", "Text"),
            ("inputChargeUnit", "Text"),
            ("inputCharge", "Int"),
            ("inputRecords", "Int"),
            ("malformed", "Text"),
            ("rejectedRecords", "Int"),
            ("rejectedInputBytes", "Int"),
            ("outputCharge", "Int"),
            ("outputRecords", "Int"),
            ("work", "Int"),
            ("workAllowance", "Int"),
            ("heldCharge", "Int"),
            ("highWaterCharge", "Int"),
            ("finishApplied", "Bool"),
            ("sourceComplete", "Option<Bool>"),
            ("failureCode", "Option<Text>"),
            ("failureMessage", "Option<Text>"),
            ("exhausted", "Option<Text>"),
            ("rejectedStart", "Option<Int>"),
            ("rejectedEnd", "Option<Int>"),
            ("analysisId", "Text"),
            ("transitionRevision", "Text"),
            ("finishRevision", "Option<Text>"),
            ("profile", "Text"),
            ("profileRevision", "Text"),
            ("sourceNode", "Option<Text>"),
            ("sourceRun", "Option<Text>"),
            ("sourceRevision", "Option<Int>"),
            ("sourcePort", "Option<Text>"),
            ("sourcePath", "List<Text>"),
            ("durableResume", "Bool"),
            ("attempt", "Option<Text>"),
            ("previousAttempt", "Option<Text>"),
            ("budgetDigest", "Option<Text>"),
            ("budgetIssuedAttempt", "Option<Text>"),
            ("budgetPrevious", "Option<Text>"),
            ("authorizedWork", "Int"),
            ("workGrant", "Int"),
            ("durationOverrunMs", "Option<Int>"),
            ("measuredWork", "Int"),
            ("outstandingWork", "Option<Int>"),
            ("durationChargedMs", "Int"),
            ("durationOutstandingMs", "Option<Int>"),
        ],
        span,
    )?;
    let ContractKind::Record(fields) = receipt.kind() else {
        unreachable!("native receipt")
    };
    let mut fields = fields.clone();
    fields.insert(
        "limits".into(),
        ContractField {
            contract: limits,
            optional: false,
        },
    );
    let receipt = Arc::new(
        Contract::record("ScanReceipt", fields)
            .map_err(|error| Failure::new("CAL002", span, error.to_string()))?,
    );
    let outputs = Arc::new(
        (if dataset {
            Contract::dataset("ScanOutputs", step.output_contract.clone())
        } else {
            Contract::list("ScanOutputs", step.output_contract.clone())
        })
        .map_err(|error| Failure::new("CAL002", span, error.to_string()))?,
    );
    Contract::record(
        "ScanResult",
        [
            (
                "state".into(),
                ContractField {
                    contract: step.state.clone(),
                    optional: false,
                },
            ),
            (
                "outputs".into(),
                ContractField {
                    contract: outputs,
                    optional: false,
                },
            ),
            (
                "receipt".into(),
                ContractField {
                    contract: receipt,
                    optional: false,
                },
            ),
        ]
        .into(),
    )
    .map(Arc::new)
    .map_err(|error| Failure::new("CAL002", span, error.to_string()))
}
fn receipt(progress: &Progress, stop: Option<&Stop>) -> Data {
    let usage = progress.usage;
    let optional = |data: Option<Data>| Data::Option(data.map(Box::new));
    Data::Record(
        [
            (
                "status".into(),
                Data::Text(
                    match progress.phase {
                        Phase::Complete => "complete",
                        Phase::Cancelled => "cancelled",
                        Phase::Stopped => "stopped",
                        _ => "running",
                    }
                    .into(),
                ),
            ),
            (
                "position".into(),
                Data::Int(progress.committed_position as i64),
            ),
            (
                "readPosition".into(),
                Data::Int(progress.read_position as i64),
            ),
            ("extent".into(), Data::Int(progress.extent as i64)),
            (
                "positionUnit".into(),
                Data::Text(if progress.framed { "bytes" } else { "records" }.into()),
            ),
            (
                "inputChargeUnit".into(),
                Data::Text(
                    if progress.framed {
                        "raw_bytes"
                    } else {
                        "logical_charge"
                    }
                    .into(),
                ),
            ),
            ("inputCharge".into(), Data::Int(usage.input_bytes as i64)),
            ("inputRecords".into(), Data::Int(usage.input_records as i64)),
            (
                "malformed".into(),
                Data::Text(
                    if progress.coverage.is_some() {
                        "forensic"
                    } else {
                        "strict"
                    }
                    .into(),
                ),
            ),
            (
                "rejectedRecords".into(),
                Data::Int(progress.coverage.as_ref().map_or(0, |c| c.records) as i64),
            ),
            (
                "rejectedInputBytes".into(),
                Data::Int(progress.coverage.as_ref().map_or(0, |c| c.input_bytes) as i64),
            ),
            ("outputCharge".into(), Data::Int(usage.output_bytes as i64)),
            (
                "outputRecords".into(),
                Data::Int(usage.output_records as i64),
            ),
            ("work".into(), Data::Int(usage.work as i64)),
            (
                "workAllowance".into(),
                Data::Int(progress.work_allowance as i64),
            ),
            ("heldCharge".into(), Data::Int(usage.held_bytes as i64)),
            (
                "highWaterCharge".into(),
                Data::Int(usage.high_water_bytes as i64),
            ),
            ("finishApplied".into(), Data::Bool(progress.finish_applied)),
            (
                "sourceComplete".into(),
                optional(progress.producer_complete.map(Data::Bool)),
            ),
            (
                "failureCode".into(),
                optional(stop.map(|stop| Data::Text(stop.failure.code.into()))),
            ),
            (
                "failureMessage".into(),
                optional(stop.map(|stop| {
                    Data::Text(
                        stop.failure
                            .message
                            .chars()
                            .take(4096)
                            .collect::<String>()
                            .into(),
                    )
                })),
            ),
            (
                "exhausted".into(),
                optional(
                    stop.and_then(|stop| stop.dimension)
                        .map(|dimension| Data::Text(dimension.name().into())),
                ),
            ),
            (
                "rejectedStart".into(),
                optional(
                    stop.and_then(|stop| stop.source_span)
                        .map(|at| Data::Int(at.start as i64)),
                ),
            ),
            (
                "rejectedEnd".into(),
                optional(
                    stop.and_then(|stop| stop.source_span)
                        .map(|at| Data::Int(at.end as i64)),
                ),
            ),
        ]
        .into(),
    )
}
