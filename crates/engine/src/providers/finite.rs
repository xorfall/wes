//! Single-use validated finite work, before auxiliary-source dispatch or a Calling receipt.
use super::*;
use crate::storage::ValueLease;
#[cfg(test)]
mod tests;

pub type FinitePreparationFuture =
    Pin<Box<dyn Future<Output = Result<PreparedCall, ExecutionReport>> + Send + 'static>>;

/// Preparation performs validation and native read setup, never external invocation.
/// Keep this owner until execution; it cannot be cloned or reconstructed from stored data.
#[must_use = "execute or discard the prepared finite call"]
pub struct PreparedCall {
    bound: BoundCall,
    run: Run,
    arguments: IndexMap<String, Value>,
    carried: Provenance,
    read_access: read_access::Attempt,
    dispatch: Option<crate::provider_reads::ReadGuard>,
    input_source: Option<crate::byte_input::CapturedSource>,
    byte_input: Option<crate::byte_input::OwnedByteInput>,
    journal: Option<CallJournal>,
    storage: Option<crate::storage::StoreWorker>,
    cancellation: CancellationToken,
    policy: wes_core::flow::FlowPolicy,
    input_holds: Vec<ValueLease>,
}
impl std::fmt::Debug for PreparedCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedCall")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

pub(super) fn prepare_call(
    journal: Option<CallJournal>,
    storage: Option<crate::storage::StoreWorker>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> FinitePreparationFuture {
    let policy = ticket.payload.failure_policy(&ticket.inputs);
    Box::pin(async move {
        let input_holds = ticket.input_holds;
        let work = async {
            if ticket.payload.streaming() || ticket.payload.interactive() {
                return Err(Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "This call requires its stream or conversation execution port.",
                    None,
                ))
                .into());
            }
            // Validation can walk large values. Keep it off asynchronous scheduler threads, and
            // join its physical exit even when cancelled, just like any other blocking work.
            let validation_cancel = cancellation.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                prepare(
                    &ticket.payload.invocation,
                    &ticket.inputs,
                    &validation_cancel,
                )
                .map(|(arguments, mut carried)| {
                    // A pipe remains a control/data dependency even when the stage does not
                    // project any fields. Do not shed its private/transfer restrictions.
                    if let Some(input) = &ticket.payload.context.pipe_input
                        && let Some(value) = ticket.inputs.get(&input.node)
                    {
                        carried = carried.with_policy(value.provenance().policy());
                    }
                    (ticket.payload, ticket.run, arguments, carried)
                })
            })
            .await;
            let (bound, run, arguments, carried) = match prepared {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => return Err(error.outcome().into()),
                Err(_) => {
                    return Err(Outcome::Failed(
                        RuntimeCode::ExecutionFailed
                            .error("Input validation terminated unexpectedly.", None),
                    )
                    .into());
                }
            };
            let read_access = match bound.read_access(&arguments) {
                Ok(access) => access,
                Err(error) => return Err(error.outcome().into()),
            };
            let dispatch = bound
                .dispatch_guard()
                .map_err(|error| ExecutionReport::from(error.outcome()))?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled.outcome().into());
            }
            let input_source = bound.context.byte_input.clone();
            let byte_input = if let Some(spec) = bound.byte_input_spec() {
                if !bound.context.authority.is_local_user() {
                    return Err(Outcome::Failed(RuntimeCode::ResultForbidden.error("Original byte handoff requires an explicit native user invocation; read access alone is insufficient.", None)).into());
                }
                if let Err(reason) = bound.require_dispatch(&carried) {
                    return Err(environment_denied(reason).into());
                }
                let Some(source) = input_source.clone() else {
                    return Err(Outcome::Failed(RuntimeCode::InputFailed.error("Original byte input was not captured at native entry; no provider was invoked.", None)).into());
                };
                match source.open(storage.clone(), spec.maximum).await {
                    Ok(input) => Some(input),
                    Err(error) => return Err(error.outcome().into()),
                }
            } else {
                None
            };
            match (&journal, &bound.context.admission) {
                (Some(journal), Some(admission))
                    if journal.validate_admission(admission, &run).is_ok() => {}
                (None, None) => {}
                _ => return Err(recording_denied(&cancellation)),
            }
            if let Err(outcome) = check_prepared(
                &bound,
                &carried,
                &read_access,
                &dispatch,
                &input_source,
                &cancellation,
                None,
            ) {
                return Err(outcome.into());
            }
            Ok(PreparedCall {
                bound,
                run,
                arguments,
                carried,
                read_access,
                dispatch,
                input_source,
                byte_input,
                journal,
                storage,
                cancellation,
                policy: policy.clone(),
                input_holds,
            })
        };
        work.await.map_err(|mut report: ExecutionReport| {
            report.outcome = report.outcome.with_policy(&policy);
            for notice in &mut report.notices {
                *notice = notice.clone().with_policy(&policy);
            }
            report
        })
    })
}

impl PreparedCall {
    pub fn run(&self) -> &Run {
        &self.run
    }
    pub fn execute(self) -> ExecutionFuture {
        self.execute_guarded(None)
    }
    /// Admit one dispatch against a native, original ready source. The embedding
    /// owner must still retain and join that source separately on every path.
    /// This is control admission, not consumed input: source-related refusals
    /// retain their policy, while ordinary results inherit actual inputs and
    /// their own provider/read scope. No source payload is supplied here.
    pub fn execute_after(self, prerequisite: SourcePrerequisite) -> ExecutionFuture {
        self.execute_guarded(Some(prerequisite))
    }
    fn execute_guarded(self, prerequisite: Option<SourcePrerequisite>) -> ExecutionFuture {
        let Self {
            bound,
            run,
            arguments,
            carried,
            mut read_access,
            dispatch,
            input_source,
            byte_input,
            journal,
            storage,
            cancellation,
            policy,
            input_holds,
        } = self;
        let publication_policy = policy.clone();
        let work = async move {
            let byte_input_holds = byte_input
                .as_ref()
                .map(crate::byte_input::OwnedByteInput::retain_owner);
            if let Err(outcome) = check_prepared(
                &bound,
                &carried,
                &read_access,
                &dispatch,
                &input_source,
                &cancellation,
                prerequisite.as_ref(),
            ) {
                return outcome.into();
            }
            let calling = match lifecycle::begin(&journal, &bound, &run, &cancellation).await {
                Ok(calling) => calling,
                Err(report) => return *report,
            };
            // An accepted writer request is joined even when cancellation happens while waiting.
            // In that case close this local attempt without ever entering the provider.
            let protected = read_access
                .protect(bound.output_provenance(&carried))
                .with_policy(&publication_policy);
            let trace = if let Some(profile) = bound.invocation.trace_profile.as_deref() {
                bound
                    .context
                    .traces
                    .as_ref()
                    .and_then(|traces| traces.begin(&run, profile, protected.clone()))
            } else {
                None
            };
            let acquisition = storage
                .clone()
                .filter(|s| {
                    s.transient_identity().is_some()
                        && !read_access.is_probe()
                        && !protected.policy().is_private()
                        && !protected.policy().is_unknown()
                })
                .map(|s| {
                    crate::storage::AcquisitionContext::new(
                        s,
                        protected
                            .clone()
                            .cautioned(bound.invocation.cautions.iter().cloned()),
                    )
                });
            let mut output_hold = None;
            let observation = trace.clone();
            let outcome = if bound.invocation.trace_profile.is_some() && trace.is_none() {
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Trace capacity is unavailable; the provider was not called.",
                    None,
                ))
            } else if let Err(reason) = bound.require_dispatch(&carried) {
                environment_denied(reason)
            } else if cancellation.is_cancelled() {
                InvocationError::Cancelled.outcome()
            } else if !read_access.current() {
                read_access::refusal().outcome()
            } else {
                let entry = Entry {
                    bound: bound.clone(),
                    carried: carried.clone(),
                    prerequisite,
                    dispatch,
                };
                let provider = bound.provider;
                let call = Call {
                    authority: bound.context.authority.clone(),
                    run,
                    capability: bound.invocation.capability.clone(),
                    arguments,
                };
                // Join the actual provider future, including synchronous invoke panics. A panic
                // payload may contain secrets and is never copied into a value or notice.
                let probe_cancellation = cancellation.clone();
                let invocation_cancel = cancellation.child_token();
                let source_guard = input_source
                    .clone()
                    .map(|source| tokio::spawn(source.guard(invocation_cancel.clone())));
                let joined_cancel = invocation_cancel.clone();
                let read_access_guard = read_access.dispatch_guard();
                let input_source_guard = input_source.clone();
                let mut result = tokio::spawn(async move {
                    let cancellation = invocation_cancel;
                    entry.admit(
                        &cancellation,
                        &read_access_guard,
                        input_source_guard.as_ref(),
                    )?;
                    if let Some(input) = byte_input {
                        return provider
                            .invoker
                            .invoke_with_byte_input(call, cancellation, input)
                            .await
                            .map(crate::storage::AcquiredOutput::Value);
                    }
                    match observation {
                        Some(trace) => provider
                            .invoker
                            .invoke_observed(call, cancellation, trace)
                            .await
                            .map(crate::storage::AcquiredOutput::Value),
                        None => match acquisition {
                            Some(context) => {
                                provider
                                    .invoker
                                    .invoke_acquired(call, cancellation, context)
                                    .await
                            }
                            None => provider
                                .invoker
                                .invoke(call, cancellation)
                                .await
                                .map(crate::storage::AcquiredOutput::Value),
                        },
                    }
                })
                .await;
                joined_cancel.cancel();
                if let Some(guard) = source_guard {
                    let _ = guard.await;
                }
                if let (Some(source), Some(worker)) = (&input_source, &storage)
                    && let Ok(Ok(crate::storage::AcquiredOutput::Value(value))) = &result
                {
                    let value = value.with_provenance(value.provenance().inheriting(&protected));
                    result = Ok(source.publish_derived(worker, value).await);
                }
                let source_current = input_source.as_ref().is_none_or(|s| s.check().is_ok());
                if !source_current {
                    read_access::refusal().outcome()
                } else {
                    match result {
                        Ok(Ok(crate::storage::AcquiredOutput::Stored { output, hold })) => {
                            if read_access.current() && !probe_cancellation.is_cancelled() {
                                output_hold = Some(hold);
                                Outcome::Stored(output)
                            } else {
                                read_access::refusal().outcome()
                            }
                        }
                        Ok(Ok(crate::storage::AcquiredOutput::Value(value))) => {
                            match read_access.finish(&value, probe_cancellation.is_cancelled()) {
                                Err(error) => error.outcome(),
                                Ok(()) => {
                                    let provenance = value
                                        .provenance()
                                        .inheriting(&protected)
                                        .cautioned(bound.invocation.cautions.iter().cloned());
                                    Outcome::Produced(if &provenance == value.provenance() {
                                        value
                                    } else {
                                        value.with_provenance(provenance)
                                    })
                                }
                            }
                        }
                        Ok(Err(error)) => error.outcome(),
                        Err(_) => Outcome::Failed(
                            RuntimeCode::ExecutionFailed
                                .error("The provider terminated unexpectedly.", None),
                        ),
                    }
                }
            };
            let mut report: ExecutionReport = outcome.with_policy(protected.policy()).into();
            if let Some(hold) = output_hold {
                report
                    .holds
                    .push(crate::driver::ExecutionHold::retain(hold));
            }
            if let Outcome::Failed(error) = &report.outcome
                && error.code() == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
            {
                // Cancellation can revoke node publication before physical exec returns. Keep
                // remote uncertainty as an operational notice even for an obsolete run.
                report.notices.push(error.clone());
            }
            if let Some(trace) = trace {
                let (state, code, provenance) = match &report.outcome {
                    Outcome::Produced(value) => ("completed", "", value.provenance().clone()),
                    Outcome::Failed(error) | Outcome::Incomplete { error, .. } => (
                        "failed",
                        error.code(),
                        wes_core::Provenance::default().with_policy(error.policy()),
                    ),
                    _ => ("cancelled", "", protected.clone()),
                };
                trace.emit(
                    "execution.finished",
                    crate::trace::record(
                        "ExecutionCompletion",
                        [
                            ("state".into(), crate::trace::text(state)),
                            ("code".into(), crate::trace::text(code)),
                        ],
                        provenance,
                    ),
                );
                trace.finish(match &report.outcome {
                    Outcome::Produced(_) | Outcome::Stored(_) => "completed",
                    Outcome::Failed(_) | Outcome::Incomplete { .. } => "failed",
                    _ => "cancelled",
                });
                if let Some(record) = trace.persistent_record() {
                    if let Some(journal) = &journal {
                        if journal.record_trace(record).await.is_ok() {
                            trace.persistence("recorded");
                        } else {
                            trace.persistence("failed");
                            report.notices.push(RuntimeCode::RecordingFailed.error(
                                "Inspection completed, but its trace could not be recorded.",
                                None,
                            ));
                        }
                    }
                } else {
                    trace.persistence("private");
                }
            }
            let mut report = lifecycle::finish(journal, calling, report).await;
            // Recovery acknowledgement is joined too; withdrawal/regrant during that
            // acknowledgement cannot resurrect an already captured byte-input attempt.
            if let Some(source) = &input_source
                && let Err(error) = source.check()
            {
                report.outcome = Outcome::Failed(
                    error.read_error("Original byte input authority changed before publication"),
                )
                .with_policy(protected.policy());
            }
            drop(byte_input_holds);
            report
        };
        Box::pin(async move {
            let mut report: ExecutionReport = work.await;
            report.outcome = report.outcome.with_policy(&policy);
            for notice in &mut report.notices {
                *notice = notice.clone().with_policy(&policy);
            }
            report
                .holds
                .push(crate::driver::ExecutionHold::retain(input_holds));
            report
        })
    }
}

fn check_prepared(
    bound: &BoundCall,
    carried: &Provenance,
    access: &read_access::Attempt,
    dispatch: &Option<crate::provider_reads::ReadGuard>,
    input_source: &Option<crate::byte_input::CapturedSource>,
    cancellation: &CancellationToken,
    prerequisite: Option<&SourcePrerequisite>,
) -> Result<(), Outcome> {
    if cancellation.is_cancelled() {
        return Err(InvocationError::Cancelled.outcome());
    }
    if dispatch.as_ref().is_some_and(|guard| !guard.current()) {
        return Err(InvocationError::Cancelled.outcome());
    }
    bound
        .require_dispatch(carried)
        .map_err(environment_denied)?;
    if !access.current() {
        return Err(read_access::refusal().outcome());
    }
    if let Some(source) = input_source {
        source
            .check()
            .map_err(|e| Outcome::Failed(e.read_error("Original byte input admission refused")))?;
    }
    if let Some(prerequisite) = prerequisite {
        prerequisite.check().map_err(InvocationError::outcome)?;
    }
    Ok(())
}

struct Entry {
    bound: BoundCall,
    carried: Provenance,
    prerequisite: Option<SourcePrerequisite>,
    dispatch: Option<crate::provider_reads::ReadGuard>,
}
impl Entry {
    fn admit(
        self,
        cancellation: &CancellationToken,
        read: &Option<crate::provider_reads::ReadGuard>,
        source: Option<&crate::byte_input::CapturedSource>,
    ) -> Result<(), InvocationError> {
        let admit = || {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if self.dispatch.as_ref().is_some_and(|guard| !guard.current()) {
                return Err(InvocationError::Cancelled);
            }
            self.bound
                .require_dispatch(&self.carried)
                .map_err(|reason| {
                    let Outcome::Failed(error) = environment_denied(reason) else {
                        unreachable!()
                    };
                    InvocationError::Failed(error)
                })?;
            if read.as_ref().is_some_and(|guard| !guard.current()) {
                return Err(read_access::refusal());
            }
            if let Some(source) = source {
                source.check().map_err(|e| {
                    InvocationError::Failed(e.read_error("Original byte input admission refused"))
                })?;
            }
            Ok(())
        };
        match &self.prerequisite {
            Some(prerequisite) => prerequisite.admit(admit),
            None => admit(),
        }
    }
}
