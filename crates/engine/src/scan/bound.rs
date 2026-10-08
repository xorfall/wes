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
        if literal("mode", Some("complete"))? != "complete"
            || literal("sink", Some("memory"))? != "memory"
            || literal("budget", Some("Investigation"))? != "Investigation"
        {
            return Err(invalid("finite scan supports mode:complete, sink:memory and budget:Investigation; durable/live modes are unavailable".into()));
        }
        let profile_name = literal("profile", None)?;
        let profile = scan::Profile::lookup(&profile_name)
            .ok_or_else(|| invalid("unknown native scan profile".into()))?;
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
        let framing = if profile == scan::Profile::TypedRecords {
            None
        } else {
            Some(Profile {
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
        let settings = Settings::capture();
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
        let result_shape = super::runner::result_contract(&step, span)
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
        })
    }
    pub(crate) fn with_pipe_input(mut self, input: Option<&OutputRef>) -> Self {
        self.pipe_input = input.cloned();
        self
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
        token: CancellationToken,
        progress: crate::driver::progress::Reporter,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let span = self.span;
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
                Runner::new_admitted(ScanInput {source,initial,context,step:self.step,finish:self.finish,framing:self.framing,identity,control},
                    self.settings,ledger,self.services,cancel,span)
            }).await;
            let mut runner = match initialized {
                Ok(Ok(runner)) => runner,
                Ok(Err(failure)) => return failed(failure).into(),
                Err(_) => return panic_failure(span).into(),
            };
            progress.report(runner.execution_progress());
            let mut reported = std::time::Instant::now();
            loop {
                // The physical worker is always joined, including cancellation.
                // Bound slices amortize handoff without retaining record VMs.
                let polled = tokio::task::spawn_blocking(move || {
                    let mut terminal = false;
                    for _ in 0..32 {
                        if matches!(runner.poll(), Poll::Terminal) {
                            terminal = true;
                            break;
                        }
                    }
                    (runner, terminal)
                })
                .await;
                let terminal;
                (runner, terminal) = match polled {
                    Ok(pair) => pair,
                    Err(_) => return panic_failure(span).into(),
                };
                if terminal || reported.elapsed() >= std::time::Duration::from_millis(200) {
                    progress.report(runner.execution_progress());
                    reported = std::time::Instant::now();
                }
                if terminal {
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
        })
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
fn failed(failure: Failure) -> Outcome {
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
    hash.update(b"wes.framing.v1");
    hash.update(profile.name().as_bytes());
    if let Some(profile) = framing {
        hash.update((profile.raw_bytes as u64).to_le_bytes());
        hash.update((profile.decoded_bytes as u64).to_le_bytes());
        hash.update((profile.spans as u64).to_le_bytes());
        let delimiter = match &profile.delimiter {
            Delimiter::Lines => &b"\n"[..],
            Delimiter::Literal(bytes) => bytes,
        };
        hash.update((delimiter.len() as u64).to_le_bytes());
        hash.update(delimiter);
    }
    format!("sha256:{:x}", hash.finalize())
}
