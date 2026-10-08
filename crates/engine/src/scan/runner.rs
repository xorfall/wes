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
/// byte figures except original source positions are conservative logical
/// charges, never wire-byte counts or a measured resident-memory promise.
#[derive(Clone, Copy, Debug)]
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
}
impl Settings {
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
            outputs_per_record: get("scan.record.outputs").min(limits.output_records),
            patterns: get("scan.patterns") as usize,
            block: get("scan.block.bytes") as usize,
            duration: Duration::from_millis(get("scan.duration.ms")),
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
            && self.outputs_per_record > 0
            && self.outputs_per_record <= self.limits.output_records
            && (1..=64).contains(&self.patterns)
            && (1..=65_536).contains(&self.block)
            && !self.duration.is_zero()
            && self.duration <= Duration::from_secs(86_400)
    }
}

pub struct Input {
    pub source: Value,
    pub initial: Value,
    pub context: Value,
    pub step: Transition,
    pub finish: Option<Transition>,
    pub framing: Option<Profile>,
    pub identity: Identity,
    pub control: Option<Provenance>,
}
#[derive(Clone, Debug)]
pub struct SourceIdentity {
    pub node: String,
    pub run: String,
    pub revision: u64,
    pub port: String,
    pub path: Vec<String>,
}
#[derive(Clone, Debug)]
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
    Complete,
    Stopped,
    Cancelled,
}
#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    /// Counts include only accepted candidates, not the in-flight invocation.
    pub usage: Usage,
    pub committed_position: u64,
    pub read_position: u64,
    pub extent: u64,
    pub framed: bool,
    pub finish_applied: bool,
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
    committed_position: u64,
    invocation: Option<Invocation>,
    cache: Option<IterRegexCache>,
    regex: wes_core::RegexCacheUsage,
    provenance: Provenance,
    result_contract: Arc<Contract>,
    result_metadata: ValueMetadata,
    services: Option<Arc<dyn LocalServices>>,
    token: CancellationToken,
    started: Instant,
    span: Span,
    phase: Phase,
    finish_applied: bool,
    stop: Option<Stop>,
    identity: Identity,
}
impl Runner {
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
        input: Input,
        settings: Settings,
        mut ledger: Ledger,
        services: Option<Arc<dyn LocalServices>>,
        token: CancellationToken,
        span: Span,
    ) -> Result<Self, Failure> {
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
        // Native schema wrappers/metadata and terminal receipt have a bounded
        // reservation of their own, including the copy at the final handoff.
        let identity_charge = identity_charge(&input.identity, span)?;
        let base_charge = sum(
            &[
                source_charge,
                context_charge,
                identity_charge,
                input.step.code_charge,
                input.finish.as_ref().map_or(0, |f| f.code_charge),
                framing,
                cache_charge,
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
                &source_end(source.total(), source.framed(), provenance.clone()),
                &|| token.is_cancelled(),
                span,
            )?;
        }
        let result_contract = result_contract(&input.step, span)?;
        let result_metadata = ValueMetadata::capture(&result_contract);
        if result_metadata.charge() > 128 * 1024 {
            return Err(Failure::new(
                "CAL006",
                span,
                "scan result declaration exceeds its metadata reservation",
            ));
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
            committed_position: 0,
            invocation: None,
            cache: Some(cache),
            regex: Default::default(),
            provenance,
            result_contract,
            result_metadata,
            services,
            token,
            started: Instant::now(),
            span,
            phase: Phase::Reading,
            finish_applied: false,
            stop: None,
            identity: input.identity,
        })
    }
    pub fn progress(&self) -> Progress {
        Progress {
            phase: self.phase,
            usage: self.ledger.usage(),
            committed_position: self.committed_position,
            read_position: self.source.position(),
            extent: self.source.total(),
            framed: self.source.framed(),
            finish_applied: self.finish_applied,
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
        if self.started.elapsed() >= self.settings.duration {
            return self.fail(Stop {
                failure: Failure::new("CAL006", self.span, "scan duration limit reached"),
                dimension: None,
                source_span: None,
            });
        }
        match self.poll_inner() {
            Ok(poll) => poll,
            Err(stop) => self.fail(stop),
        }
    }
    fn fail(&mut self, mut stop: Stop) -> Poll {
        stop.failure.policy = stop.failure.policy.join(self.provenance.policy());
        self.provenance = self.provenance.clone().with_policy(&stop.failure.policy);
        self.phase = if stop.failure.cancelled {
            Phase::Cancelled
        } else {
            Phase::Stopped
        };
        self.invocation = None;
        self.stop = Some(stop);
        // No finish after failure, no callback retry, no partial candidate commit.
        let held = self
            .base_charge
            .saturating_add(self.state_charge)
            .saturating_add(self.ledger.usage().output_bytes);
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
                    let id = request.id();
                    let result = match request {
                        Request::Call { span, .. } => {
                            return Err(stop(Failure::new(
                                "CAL002",
                                span,
                                "pure scan requested external execution; no provider was entered",
                            )));
                        }
                        Request::ParseJson {
                            text,
                            contract,
                            span,
                            ..
                        } => {
                            active
                                .machine
                                .charge_native_work(
                                    (text.len() as u64).saturating_mul(16).saturating_add(1024),
                                    span,
                                )
                                .map_err(stop)?;
                            self.services
                                .as_ref()
                                .ok_or_else(|| {
                                    stop(Failure::new(
                                        "CAL004",
                                        span,
                                        "scan JSON local service is unavailable",
                                    ))
                                })?
                                .read(&text, contract.as_deref(), &self.token, span)
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
                        self.state_charge,
                        self.ledger.usage().output_bytes,
                        self.settings.record_charge,
                    ],
                    self.span,
                )
                .map_err(stop)?,
            )
            .map_err(|error| refusal(error, self.span))?;
        let ledger = &mut self.ledger;
        match self
            .source
            .poll_admitted(self.settings.block, &self.token, self.span, |amount| {
                ledger.work(amount)
            })
            .map_err(|error| Stop {
                failure: error.failure,
                dimension: error.dimension,
                source_span: error.source_span,
            })? {
            SourcePoll::Pending => Ok(Poll::Yield),
            SourcePoll::Record {
                value,
                start,
                end,
                input_charge,
            } => {
                self.ledger
                    .admit_input(input_charge)
                    .map_err(|error| refusal(error, self.span))?;
                self.begin(value, Some((start, end, input_charge)), false)?;
                Ok(Poll::Yield)
            }
            SourcePoll::End => {
                if self.finish.is_some() {
                    let end = source_end(
                        self.source.position(),
                        self.source.framed(),
                        self.provenance.clone(),
                    );
                    self.begin(end, None, true)?;
                    Ok(Poll::Yield)
                } else {
                    self.phase = Phase::Complete;
                    self.ledger
                        .held(
                            sum(
                                &[
                                    self.base_charge,
                                    self.state_charge,
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
                    .saturating_add(self.state_charge)
                    .saturating_add(1024),
            )
            .map_err(|error| refusal(error, self.span))?;
        self.ledger
            .held(
                sum(
                    &[
                        self.base_charge,
                        self.state_charge,
                        self.ledger.usage().output_bytes,
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
            ("state".into(), self.state.clone()),
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
            let amount = crate::value_size::data_charge(item, self.settings.limits.output_bytes)
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
        if self.started.elapsed() >= self.settings.duration {
            return Err(stop(Failure::new(
                "CAL006",
                self.span,
                "scan duration limit reached before candidate commit",
            )));
        }
        let held_after = sum(
            &[
                self.base_charge,
                state_charge,
                self.ledger.usage().output_bytes,
                output_charge,
            ],
            self.span,
        )
        .map_err(stop)?;
        // Reserve possible Vec geometric capacity before appending. Record Data
        // charge includes container overhead; no per-record graph/history nodes.
        self.ledger
            .commit(
                range.map(|(_, _, charge)| charge),
                outputs.len() as u64,
                output_charge,
                held_after,
            )
            .map_err(|error| refusal(error, self.span))?;
        self.state = state;
        self.state_charge = state_charge;
        self.outputs.extend(outputs);
        self.provenance = provenance;
        if let Some((_, end, charge)) = range {
            self.committed_position = end;
            self.ledger
                .earn(charge.saturating_mul(self.settings.work_per_input_unit));
        }
        self.finish_applied = finishing;
        Ok(())
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
        let mut receipt = receipt(&progress, self.stop.as_ref());
        let Data::Record(fields) = &mut receipt else {
            unreachable!()
        };
        let optional = |value: Option<Data>| Data::Option(value.map(Box::new));
        fields.insert(
            "analysisId".into(),
            Data::Text(self.identity.analysis.into()),
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
        fields.insert("profile".into(), Data::Text(self.identity.profile.into()));
        fields.insert(
            "profileRevision".into(),
            Data::Text(self.identity.profile_revision.into()),
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
            Data::List(self.identity.source.map_or_else(Vec::new, |s| {
                s.path.into_iter().map(|p| Data::Text(p.into())).collect()
            })),
        );
        fields.insert("durableResume".into(), Data::Bool(false));
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
        let value = Value::new(
            self.result_contract.shape(),
            Data::Record(
                [
                    ("state".into(), self.state.into_data()),
                    ("outputs".into(), Data::List(self.outputs)),
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
fn stop(failure: Failure) -> Stop {
    let dimension = failure.budget.map(|kind| match kind {
        calc::BudgetKind::Work => Dimension::RecordWork,
        calc::BudgetKind::CumulativeWork => Dimension::Work,
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
fn source_end(position: u64, framed: bool, provenance: Provenance) -> Value {
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
                ("kind".into(), Data::Text("selected_range".into())),
                ("position".into(), Data::Int(position as i64)),
                (
                    "unit".into(),
                    Data::Text(if framed { "bytes" } else { "records" }.into()),
                ),
                ("producerComplete".into(), Data::Option(None)),
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
        Contract::list("ScanOutputs", step.output_contract.clone())
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
                        _ => "stopped",
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
            ("sourceComplete".into(), Data::Option(None)),
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
