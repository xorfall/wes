//! Normal workspace admission for finite analysis. Definition/profile/settings
//! resolution happens once; record invocations cannot acquire ambient authority.
use super::{
    Identity, Input as ScanInput, Poll, Runner, Settings, SourceIdentity, Transition,
    ledger::MemoryPool,
};
use crate::{
    calc::{Failure, LocalServices},
    driver::{CancellationToken, ExecutionFuture},
    graph::OutputRef,
    plan::{Input, MetaTask},
    runtime::{Outcome, RunTicket},
};
use std::sync::Arc;
use wes_core::{
    Data, ErrorId, ErrorValue, Shape,
    capability::Typing,
    framing::{Decoding, Delimiter, Profile},
};
use wes_language::{Diagnostic, Span, templates::Templates, vocabulary::scan};

#[derive(Clone)]
pub struct BoundScan {
    source: Input,
    initial: Input,
    context: Input,
    step: Transition,
    finish: Option<Transition>,
    framing: Option<Profile>,
    profile: scan::Profile,
    profile_revision: String,
    settings: Settings,
    services: Option<Arc<dyn LocalServices>>,
    span: Span,
    result_shape: Shape,
    pipe_input: Option<OutputRef>,
    durable_sink: bool,
    live: bool,
}
impl std::fmt::Debug for BoundScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundScan")
            .field("profile", &self.profile)
            .field("transition_revision", &self.step.revision())
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}
impl BoundScan {
    pub(crate) fn durable(&self) -> bool {
        self.durable_sink
    }
    /// Supply the captured declaration's shapes to the common argument planner.
    /// Written literals are parsed contextually there; referenced values retain
    /// their producing type and are checked unchanged at dispatch.
    pub(crate) fn expected_arguments(
        call: &wes_language::Call,
        templates: &Templates,
    ) -> Result<indexmap::IndexMap<String, Shape>, Diagnostic> {
        let argument = call
            .arguments
            .iter()
            .find(|argument| argument.key.text == "transition")
            .ok_or_else(|| {
                Diagnostic::error(
                    "CAL009",
                    call.span,
                    "scan requires transition: naming a declared pure definition",
                )
            })?;
        let name = match &argument.value {
            wes_language::Value::Word(name) | wes_language::Value::Text(name) => &name.text,
            _ => {
                return Err(Diagnostic::error(
                    "CAL009",
                    argument.span,
                    "scan transition: must be a literal definition name",
                ));
            }
        };
        let definition = templates.snapshot().get(name).ok_or_else(|| {
            Diagnostic::error(
                "CAL009",
                argument.span,
                format!("scan definition '{name}' is absent; declare it before using it"),
            )
        })?;
        let step = Transition::capture(
            definition,
            false,
            wes_budgets::get("scan.code.bytes"),
            argument.span,
        )
        .map_err(|failure| Diagnostic::error(failure.code, argument.span, failure.message))?;
        Ok([
            ("initial".into(), step.state.shape()),
            ("context".into(), step.context.shape()),
        ]
        .into())
    }
    pub(crate) fn bind(
        task: MetaTask,
        templates: &Templates,
        services: Option<Arc<dyn LocalServices>>,
        span: Span,
    ) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("CAL009", span, message);
        let required = |name: &str| {
            task.inputs
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("scan requires {name}:")))
        };
        let literal = |name: &str, default: Option<&str>| -> Result<String, Diagnostic> {
            match task.inputs.get(name) {
                None => default
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("scan requires literal {name}:"))),
                Some(Input::Literal(value)) => match value.data() {
                    Data::Text(text) => Ok(text.to_string()),
                    _ => Err(invalid(format!("scan {name}: must be literal text"))),
                },
                _ => Err(invalid(format!(
                    "scan {name}: is a captured declaration and must be literal text"
                ))),
            }
        };
        if !task.tail.is_empty() || !task.subjects.is_empty() {
            return Err(invalid(
                "scan uses named arguments, without positional operands".into(),
            ));
        }
        let sink = literal("sink", Some("memory"))?;
        let mode = literal("mode", Some("complete"))?;
        let follow = match task.inputs.get("follow") {
            None => false,
            Some(Input::Literal(value)) => match value.data() {
                Data::Bool(value) => *value,
                _ => return Err(invalid("scan follow: must be a literal boolean".into())),
            },
            _ => return Err(invalid("scan follow: must be a literal boolean".into())),
        };
        let budget = literal(
            "budget",
            Some(if follow {
                "LiveAnalysis"
            } else {
                "Investigation"
            }),
        )?;
        if mode != "complete"
            || !matches!(sink.as_str(), "memory" | "dataset")
            || budget
                != if follow {
                    "LiveAnalysis"
                } else {
                    "Investigation"
                }
        {
            return Err(invalid("scan uses mode:complete and sink:memory|dataset; follow:true requires budget:LiveAnalysis, otherwise budget:Investigation".into()));
        }
        let profile_name = literal("profile", None)?;
        let profile = scan::Profile::lookup(&profile_name)
            .ok_or_else(|| invalid("unknown native scan profile".into()))?;
        if follow && (sink != "dataset" || profile != scan::Profile::TypedRecords) {
            return Err(invalid("follow:true requires sink:dataset and profile:TypedRecords; it reads recorded EventLog extensions only".into()));
        }
        let delimiter = if profile == scan::Profile::DelimitedUtf8 {
            let delimiter = literal("delimiter", None)?;
            if delimiter.is_empty() || delimiter.len() > 4096 {
                return Err(invalid(
                    "scan delimiter must contain 1–4096 UTF-8 bytes".into(),
                ));
            }
            Some(delimiter.into_bytes())
        } else {
            if task.inputs.contains_key("delimiter") {
                return Err(invalid(
                    "delimiter: is only valid with profile:DelimitedUtf8".into(),
                ));
            }
            None
        };
        let malformed = literal("malformed", Some("strict"))?;
        if !matches!(malformed.as_str(), "strict" | "forensic") {
            return Err(invalid("scan malformed: must be strict or forensic".into()));
        }
        let malformed = if malformed == "forensic" {
            if sink != "dataset" || follow || profile == scan::Profile::TypedRecords {
                return Err(invalid("forensic framing requires finite Text/Bytes with sink:dataset and a framed profile; TypedRecords and follow:true are not supported".into()));
            }
            let excerpt_bytes = match task.inputs.get("excerpt") {
                None => 256,
                Some(Input::Literal(value)) => match value.data() {
                    Data::Int(n) if (1..=4096).contains(n) => *n as usize,
                    _ => {
                        return Err(invalid(
                            "scan excerpt: must be a literal integer from 1 to 4096".into(),
                        ));
                    }
                },
                _ => {
                    return Err(invalid(
                        "scan excerpt: must be a literal integer from 1 to 4096".into(),
                    ));
                }
            };
            wes_core::framing::Malformed::Forensic { excerpt_bytes }
        } else {
            if task.inputs.contains_key("excerpt") {
                return Err(invalid(
                    "scan excerpt: is only valid with malformed:forensic".into(),
                ));
            }
            wes_core::framing::Malformed::Strict {}
        };
        let framing = if profile == scan::Profile::TypedRecords {
            None
        } else {
            Some(Profile {
                malformed,
                delimiter: delimiter.map_or(Delimiter::Lines, Delimiter::Literal),
                decoding: if profile == scan::Profile::LinesLossyUtf8 {
                    Decoding::LossyUtf8
                } else {
                    Decoding::StrictUtf8
                },
                raw_bytes: wes_budgets::get("scan.frame.raw.bytes") as usize,
                decoded_bytes: wes_budgets::get("scan.frame.decoded.bytes") as usize,
                spans: wes_budgets::get("scan.frame.spans") as usize,
            })
        };
        let settings =
            super::bounds::RequestedBounds::parse(&task, span)?.fresh(Settings::capture(), span)?;
        let definition = |name: &str, finishing: bool| -> Result<Transition, Diagnostic> {
            let definition=templates.snapshot().get(name).ok_or_else(||invalid(format!("scan definition '{name}' is absent; declare it with :def and explicit parameter/output contracts")))?;
            Transition::capture(
                definition,
                finishing,
                wes_budgets::get("scan.code.bytes"),
                span,
            )
            .map_err(|failure| Diagnostic::error(failure.code, span, failure.message))
        };
        let step = definition(&literal("transition", None)?, false)?;
        let finish = if task.inputs.contains_key("finish") {
            Some(definition(&literal("finish", None)?, true)?)
        } else {
            None
        };
        let result_shape = super::runner::result_contract_for_sink(&step, sink == "dataset", span)
            .map_err(|failure| Diagnostic::error(failure.code, span, failure.message))?
            .shape();
        let profile_revision = profile_digest(profile, framing.as_ref());
        Ok(Self {
            source: required("source")?,
            initial: required("initial")?,
            context: required("context")?,
            step,
            finish,
            framing,
            profile,
            profile_revision,
            settings,
            services,
            span,
            result_shape,
            pipe_input: None,
            durable_sink: sink == "dataset",
            live: follow,
        })
    }
    pub(crate) fn with_pipe_input(mut self, input: Option<&OutputRef>) -> Self {
        self.pipe_input = input.cloned();
        self
    }
    pub(crate) fn live(&self) -> bool {
        self.live
    }
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        [&self.source, &self.initial, &self.context]
            .into_iter()
            .flat_map(|input| input.dependencies().into_iter().cloned())
            .chain(self.pipe_input.iter().cloned())
    }
    pub(crate) fn predicted_typing(&self) -> Typing {
        Typing::new(self.result_shape.clone())
    }
    pub(crate) fn literal_policy(&self) -> wes_core::flow::FlowPolicy {
        [&self.source, &self.initial, &self.context]
            .into_iter()
            .flat_map(Input::literal_values)
            .fold(
                Default::default(),
                |policy: wes_core::flow::FlowPolicy, value| {
                    policy.join(value.provenance().policy())
                },
            )
    }
    pub(crate) fn execute(
        self,
        ticket: RunTicket<()>,
        pool: MemoryPool,
        storage: Option<crate::storage::StoreWorker>,
        token: CancellationToken,
        progress: crate::driver::progress::Reporter,
        values: crate::driver::lifetime::Reporter,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let span = self.span;
            let durable_sink = self.durable_sink;
            let live = self.live;
            if durable_sink && storage.is_none() {
                return failed(Failure::new(
                    "CAL004",
                    span,
                    "dataset scan requires an owned durable storage capability",
                ))
                .into();
            }
            let cancel = token.clone();
            let initialized=tokio::task::spawn_blocking(move || {
                if cancel.is_cancelled() {return Err(Failure::cancelled(span));}
                if !self.settings.valid() {return Err(Failure::new("CAL006",span,"invalid finite scan admission"));}
                // Aggregate ownership precedes field selection/container copies,
                // not only VM/cursor construction.
                let ledger=super::ledger::Ledger::with_startup(self.settings.limits,&pool,self.settings.startup_work)
                    .map_err(|error|Failure::new("CAL006",span,format!("scan {} admission refused ({})",error.dimension.name(),error.limit)))?;
                let resolve=|name:&str,input:&Input,limit:u64|input.resolve_with_limit(&ticket.inputs,limit).map(std::borrow::Cow::into_owned)
                    .map_err(|error| {let code=match error.problem {crate::plan::InputProblem::ChargeLimit=>"CAL006",_=>"CAL004"};let mut failure=Failure::new(code,span,error.message());failure.policy=error.policy().clone();failure.issues=error.issue(name).into_iter().collect();failure});
                let source=resolve("source",&self.source,self.settings.source_charge)?;
                let initial=resolve("initial",&self.initial,self.settings.state_charge)?;
                let context=resolve("context",&self.context,self.settings.context_charge)?;
                let source_identity=if let Some(reference)=self.source.dependency() {
                    let origin=ticket.input_origins.get(&reference.node)
                        .ok_or_else(||Failure::new("CAL004",span,"scan source acknowledgement is unavailable; analysis cannot guess its producing run"))?;
                    if origin.port!=reference.port {return Err(Failure::new("CAL004",span,"scan source acknowledgement does not match its captured output port"));}
                    let path=match &self.source {Input::FieldPath {fields,..}=>fields.clone(),_=>vec![]};
                    Some(SourceIdentity {node:reference.node.to_string(),run:origin.run.to_string(),revision:origin.revision,port:origin.port.selector().into(),path})
                }else{None};
                let identity=Identity {analysis:ticket.run.id().to_string(),source:source_identity,
                    profile:self.profile.name().into(),profile_revision:self.profile_revision};
                let control=self.pipe_input.as_ref().and_then(|reference|ticket.inputs.get(&reference.node)).map(|v|v.provenance().clone());
                Runner::new_admitted(ScanInput {live,source,initial,context,step:self.step,finish:self.finish,framing:self.framing,identity,control},
                    self.settings,ledger,self.services,cancel,span)
            }).await;
            let mut runner = match initialized {
                Ok(Ok(runner)) => runner,
                Ok(Err(failure)) => return failed(failure).into(),
                Err(_) => return panic_failure(span).into(),
            };
            if live {
                let admitted = async {
                    let worker = storage.as_ref().expect("live sink requires owned storage");
                    if worker.dataset_changes().is_none() {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            "owned store has no committed-change lifetime",
                        ));
                    }
                    let source = runner.source_page_request()?.0;
                    let info = worker
                        .dataset_inspect(source)
                        .await
                        .map_err(|e| Failure::new("CAL004", span, e.to_string()))?;
                    runner.follow_source(info)
                }
                .await;
                if let Err(failure) = admitted {
                    return failed(failure).into();
                }
            }
            if durable_sink {
                let worker = storage.as_ref().expect("admitted dataset store");
                let admission = async {
                    if token.is_cancelled() {
                        return Err(Failure::cancelled(span));
                    }
                    let source = worker
                        .capture_scan_source(runner.captured_source())
                        .await
                        .map_err(|e| Failure::new("CAL004", span, e.to_string()))?;
                    let checkpoint = runner.checkpoint_seed(source)?;
                    let mut request = runner.dataset_admission()?;
                    request.checkpoint = Some(checkpoint);
                    let admission = worker
                        .dataset_create(request)
                        .await
                        .map_err(|e| Failure::new("CAL004", span, e.to_string()))?;
                    let reference = admission.reference;
                    let checkpoint = worker
                        .dataset_checkpoint(reference.clone())
                        .await
                        .map_err(|e| Failure::new("CAL004", span, e.to_string()))?
                        .ok_or_else(|| {
                            Failure::new(
                                "CAL004",
                                span,
                                "dataset admission did not preserve its checkpoint",
                            )
                        })?;
                    runner.retain_writer(admission.lease);
                    runner.attach_durable(reference, checkpoint)
                }
                .await;
                if let Err(failure) = admission {
                    return failed(failure).into();
                }
            }
            if live {
                let initial = runner.current_prefix();
                let admitted = match initial {
                    Ok(value) => values.publish(value).await,
                    Err(_) => false,
                };
                if !admitted {
                    runner.refuse_scan(Failure::new("CAL004",span,"initial live analysis prefix was not acknowledged; analysis was not detached"));
                }
            }
            run_admitted(runner, storage, token, progress, span).await
        })
    }
}
pub(crate) async fn run_admitted(
    mut runner: Runner,
    storage: Option<crate::storage::StoreWorker>,
    token: CancellationToken,
    progress: crate::driver::progress::Reporter,
    span: Span,
) -> crate::driver::ExecutionReport {
    // Subscribe before inspecting a head. Catalog changes and writer exit cannot be lost
    // between the metadata read and waiting; wakeups confer no read/producer authority.
    let mut changes = storage.as_ref().and_then(|worker| worker.dataset_changes());
    progress.report(runner.execution_progress());
    let mut reported = std::time::Instant::now();
    loop {
        // The physical worker is always joined, including cancellation.
        // Bound slices amortize handoff without retaining record VMs.
        let polled = tokio::task::spawn_blocking(move || {
            let mut result = Poll::Yield;
            for _ in 0..32 {
                result = runner.poll();
                if !matches!(result, Poll::Yield) {
                    break;
                }
            }
            (runner, result)
        })
        .await;
        let result;
        (runner, result) = match polled {
            Ok(pair) => pair,
            Err(_) => return panic_failure(span).into(),
        };
        if matches!(result, Poll::ReadHead) {
            let Some(worker) = &storage else {
                runner.refuse_scan(Failure::new(
                    "CAL004",
                    span,
                    "live source requires its owned store",
                ));
                continue;
            };
            let Some(changed) = &mut changes else {
                runner.refuse_scan(Failure::new(
                    "CAL004",
                    span,
                    "live source has no committed-change lifetime",
                ));
                continue;
            };
            let dataset = runner
                .followed_dataset()
                .expect("ReadHead has an admitted recorded source")
                .to_owned();
            let watched_revision = changed.borrow_and_update().revision(&dataset);
            let (reference, work) = match runner.source_head_request() {
                Ok(request) => request,
                Err(error) => {
                    runner.refuse_scan(error);
                    continue;
                }
            };
            let head = match worker.eventlog_head(reference, Some(work)).await {
                Ok(head) => head,
                Err(error) => {
                    runner.refuse_source_read(error);
                    continue;
                }
            };
            let advance = runner.acknowledge_source_head(head);
            match advance {
                Err(e) => runner.refuse_scan(e),
                Ok(true) => {}
                Ok(false) => {
                    progress.report(runner.execution_progress());
                    let deadline = tokio::time::sleep(runner.remaining_duration());
                    tokio::pin!(deadline);
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = &mut deadline => break,
                            result = changed.changed() => {
                                if result.is_err() {
                                    runner.refuse_scan(Failure::new("CAL004",span,"recording store closed; source was not restarted"));
                                    break;
                                }
                                if changed.borrow_and_update().revision(&dataset) != watched_revision { break; }
                            }
                        }
                    }
                }
            }
            continue;
        }
        if matches!(result, Poll::ReadPage) {
            let Some(worker) = &storage else {
                runner.refuse_scan(Failure::new(
                    "CAL004",
                    span,
                    "dataset source requires its owned reader",
                ));
                continue;
            };
            let (reference, request) = match runner.source_page_request() {
                Ok(request) => request,
                Err(error) => {
                    runner.refuse_scan(error);
                    continue;
                }
            };
            match worker.dataset_page(reference, request).await {
                Ok(page) => {
                    if let Err(error) = runner.acknowledge_source_page(page) {
                        runner.refuse_scan(error);
                    }
                }
                Err(error) => runner.refuse_source_read(error),
            }
            continue;
        }
        if matches!(result, Poll::Grant | Poll::Settle | Poll::Commit) {
            let worker = storage
                .as_ref()
                .expect("dataset processing has its admitted owner");
            let request = match result {
                Poll::Grant => runner.durable_grant(),
                Poll::Settle => runner.durable_settlement().map_err(super::runner::stop),
                Poll::Commit => runner.dataset_candidate().map_err(super::runner::stop),
                _ => unreachable!("owned publication poll"),
            };
            let committed = match request {
                Ok(request) => {
                    let checkpoint = request.checkpoint.clone();
                    match worker.enqueue_dataset_append(request).await {
                        Ok(pending) => pending
                            .wait()
                            .await
                            .map(|reference| (reference, checkpoint)),
                        Err(error) => Err(error),
                    }
                }
                Err(failure) => {
                    runner.refuse_stop(failure);
                    continue;
                }
            };
            match committed {
                Ok((reference, checkpoint)) => {
                    let accepted = match result {
                        Poll::Grant => {
                            runner.acknowledge_grant(reference, checkpoint.expect("durable grant"))
                        }
                        Poll::Settle => runner.acknowledge_settlement(reference),
                        Poll::Commit => runner.acknowledge_dataset(reference),
                        _ => unreachable!("owned publication acknowledgement"),
                    };
                    if let Err(failure) = accepted {
                        runner.refuse_dataset(failure.to_string());
                    }
                }
                Err(error) => runner.refuse_dataset(error.to_string()),
            }
            progress.report(runner.execution_progress());
            continue;
        }
        let terminal = matches!(result, Poll::Terminal);
        if terminal || reported.elapsed() >= std::time::Duration::from_millis(200) {
            progress.report(runner.execution_progress());
            reported = std::time::Instant::now();
        }
        if terminal {
            if let Some(worker) = &storage {
                if let Ok(Some(request)) = runner.durable_terminal_update() {
                    let checkpoint = request
                        .checkpoint
                        .clone()
                        .expect("durable terminal checkpoint");
                    let written = match worker.enqueue_dataset_append(request).await {
                        Ok(pending) => pending.wait().await,
                        Err(error) => Err(error),
                    };
                    match written {
                        Ok(reference) => {
                            if let Err(failure) = runner.acknowledge_terminal(reference, checkpoint)
                            {
                                runner.refuse_dataset(failure.to_string());
                            }
                        }
                        Err(error) => runner.refuse_dataset(error.to_string()),
                    }
                }
            }
            break;
        }
        tokio::task::yield_now().await;
    }
    let final_progress = runner.execution_progress();
    let completed = tokio::task::spawn_blocking(move || runner.into_completion()).await;
    match completed {
        Ok(Ok(completion)) => {
            // Runtime cancellation/revision withdrawal still rejects late
            // publication. This does not create a cancellation exception.
            if token.is_cancelled() {
                return failed(Failure::cancelled(span)).into();
            }
            let (value, _, stop, hold) = completion.into_parts();
            let outcome = match stop {
                None => Outcome::Produced(value),
                Some(stop) if stop.failure.cancelled => failed(stop.failure),
                Some(stop) => Outcome::Incomplete {
                    value,
                    error: error(stop.failure),
                },
            };
            let mut report: crate::driver::ExecutionReport = outcome.into();
            report.progress = Some(final_progress);
            report.holds.push(hold);
            report
        }
        Ok(Err(failure)) => failed(failure).into(),
        Err(_) => panic_failure(span).into(),
    }
}
fn error(failure: Failure) -> ErrorValue {
    ErrorValue::new(
        ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("error identity"),
        failure.code,
        failure.message,
        failure.issues,
        None,
    )
    .expect("calculation failure code and issues")
    .with_policy(&failure.policy)
}
pub(crate) fn failed(failure: Failure) -> Outcome {
    if failure.cancelled {
        Outcome::Cancelled(error(failure))
    } else {
        Outcome::Failed(error(failure))
    }
}
fn panic_failure(span: Span) -> Outcome {
    failed(Failure::new(
        "CAL002",
        span,
        "record analysis worker terminated unexpectedly",
    ))
}
fn profile_digest(profile: scan::Profile, framing: Option<&Profile>) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"wes.framing.v2");
    hash.update(profile.name().as_bytes());
    if let Some(profile) = framing {
        hash.update([match profile.decoding {
            Decoding::StrictUtf8 => 0,
            Decoding::LossyUtf8 => 1,
        }]);
        hash.update([match profile.malformed {
            wes_core::framing::Malformed::Strict {} => 0,
            wes_core::framing::Malformed::Forensic { .. } => 1,
        }]);
        hash.update((profile.malformed.excerpt_bytes() as u64).to_le_bytes());
        hash.update((profile.raw_bytes as u64).to_le_bytes());
        hash.update((profile.decoded_bytes as u64).to_le_bytes());
        hash.update((profile.spans as u64).to_le_bytes());
        let delimiter = match &profile.delimiter {
            Delimiter::Lines => &b"\n"[..],
            Delimiter::Literal(bytes) => bytes,
        };
        hash.update([u8::from(matches!(
            &profile.delimiter,
            Delimiter::Literal(_)
        ))]);
        hash.update((delimiter.len() as u64).to_le_bytes());
        hash.update(delimiter);
    }
    format!("sha256:{:x}", hash.finalize())
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn recovery_decoding_delimiter_semantics_and_excerpt_bounds_have_distinct_revisions() {
        let p = Profile {
            delimiter: Delimiter::Lines,
            decoding: Decoding::StrictUtf8,
            malformed: wes_core::framing::Malformed::Strict {},
            raw_bytes: 1024,
            decoded_bytes: 4096,
            spans: 128,
        };
        let digest = |p: &Profile| profile_digest(scan::Profile::LinesUtf8, Some(p));
        let strict = digest(&p);
        let mut forensic = p.clone();
        forensic.malformed = wes_core::framing::Malformed::Forensic { excerpt_bytes: 16 };
        let first = digest(&forensic);
        assert_ne!(strict, first);
        forensic.malformed = wes_core::framing::Malformed::Forensic { excerpt_bytes: 32 };
        assert_ne!(digest(&forensic), first);
        let mut lossy = p.clone();
        lossy.decoding = Decoding::LossyUtf8;
        assert_ne!(strict, digest(&lossy));
        let mut literal = p.clone();
        literal.delimiter = Delimiter::Literal(b"\n".to_vec());
        assert_ne!(strict, digest(&literal), "literal LF does not strip CRLF");
        let strict_charge = super::super::source::framing_charge(&p).unwrap();
        assert_eq!(
            super::super::source::framing_charge(&forensic).unwrap(),
            strict_charge + 32 * 4
        );
    }
}
