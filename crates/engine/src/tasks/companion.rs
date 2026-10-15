//! One finite result and one independently joined source on the original Run.
use super::{BoundTask, source};
use crate::{
    driver::{CancellationToken, ExecutionFuture, ExecutionReport, publication::Reporter},
    eventlog::owner::Owner,
    providers::{BoundCall, CallExecutor, FinalizationLimits, InvocationError, SourceFinalization},
    runtime::{Outcome, RunTicket, RuntimeCode},
    streams::{MAX_READY_WAIT, Phase},
};
use std::time::Duration;
use wes_core::{ErrorValue, Value};

#[cfg(test)]
mod tests;

/// A native declaration, never restored control authority. The auxiliary source
/// has frozen inputs; readiness supplies no source data to the business call.
#[derive(Clone, Debug)]
pub struct BoundCompanion {
    pub(crate) call: BoundCall,
    pub(crate) source: BoundCall,
    stop: Value,
    ready_wait: Duration,
    limits: FinalizationLimits,
}
impl BoundCompanion {
    pub fn new(
        call: BoundCall,
        source: BoundCall,
        stop: Value,
        ready_wait: Duration,
        limits: FinalizationLimits,
    ) -> Result<Self, ErrorValue> {
        let task = Self {
            call,
            source,
            stop,
            ready_wait,
            limits,
        };
        task.validate()?;
        Ok(task)
    }
    fn validate(&self) -> Result<(), ErrorValue> {
        if self.call.streaming()
            || self.call.interactive()
            || !self.source.streaming()
            || self.source.interactive()
            || self.source.dependencies().next().is_some()
            || self.source.byte_input_spec().is_some()
            || self.ready_wait.is_zero()
            || self.ready_wait > MAX_READY_WAIT
            || !self.limits.valid()
            || !self.stop.data().is_inline()
            || crate::value_size::value_charge(&self.stop, 64 * 1024).is_none()
        {
            return Err(RuntimeCode::ExecutionFailed.error(
                "A finite companion requires finite business work, a frozen source, bounded readiness and an explicit typed stop; nothing was started.",
                None,
            ).with_policy(&self.call.captured_policy().join(&self.source.captured_policy()).join(self.stop.provenance().policy())));
        }
        Ok(())
    }
}

pub(super) fn execute(
    calls: CallExecutor,
    owner: Owner,
    ticket: RunTicket<BoundTask>,
    caller: CancellationToken,
    reporter: Reporter,
) -> ExecutionFuture {
    Box::pin(async move {
        let BoundTask::Companion(task) = ticket.payload else {
            unreachable!("joined companion")
        };
        if let Err(error) = task.validate() {
            return Outcome::Failed(error).into();
        }
        let BoundCompanion {
            call,
            source: auxiliary,
            stop,
            ready_wait,
            limits,
        } = *task;
        let run = ticket.run.clone();
        // Invalid business input, original byte reads or command ownership must
        // fail before any auxiliary producer can enter.
        let business = match calls
            .prepare_finite(
                RunTicket {
                    payload: call,
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                    deferred_inputs: ticket.deferred_inputs,
                    prepared_fields: ticket.prepared_fields,
                    input_holds: ticket.input_holds,
                },
                caller.clone(),
            )
            .await
        {
            Ok(call) => call,
            Err(report) => return report,
        };
        if caller.is_cancelled() {
            return business.execute().await;
        }
        let business_policy = business.admission_policy();
        let source_cancellation = CancellationToken::new();
        let prepared = match join_preparation(
            calls.prepare_source(
                RunTicket {
                    payload: auxiliary,
                    run: run.clone(),
                    inputs: Default::default(),
                    input_origins: Default::default(),
                    deferred_inputs: Default::default(),
                    prepared_fields: Default::default(),
                    input_holds: vec![],
                },
                source_cancellation.clone(),
            ),
            &caller,
            &source_cancellation,
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(report) => return protect_refusal(report, &business_policy),
        };
        let policy = prepared.policy().join(&business_policy);
        let plan = match prepared.prepare_stop(stop, limits) {
            Ok(plan) => plan,
            Err(error) => return refusal(error, &policy),
        };
        if caller.is_cancelled() {
            return business.execute().await;
        }
        let attachment_refused = protect_refusal(prepared.refusal(
            "The original joined run refused auxiliary source attachment; the business call was not invoked. Nothing was replayed.".into(),
        ), &business_policy);
        let source = match join_preparation(
            source::start(owner, run, prepared, None),
            &caller,
            &source_cancellation,
        )
        .await
        {
            Ok(source) => source,
            Err(report) => return protect_refusal(report, &business_policy),
        };
        if !reporter.attach_source(source.handle()).await {
            let _ = source.shutdown().await;
            return attachment_refused;
        }
        let prerequisite = match source.wait_ready(ready_wait, caller.clone()).await {
            Ok(ready) => ready,
            Err(error) => {
                let _ = source.shutdown().await;
                return refusal(error, &policy);
            }
        };
        let stop = match source.bind_stop(plan, &prerequisite) {
            Ok(stop) => stop,
            Err(error) => {
                let _ = source.shutdown().await;
                return refusal(error, &policy);
            }
        };
        let mut work = business.execute_after(prerequisite);
        let (mut report, finalization) = tokio::select! { biased;
            () = caller.cancelled() => {
                // Cleanup begins immediately; a cancellation-cooperative or held
                // business future still physically joins and is never replayed.
                let business = async {
                    let mut report = work.await;
                    if reporter.publish(&report).await { report.notices.clear(); }
                    report
                };
                tokio::join!(business, source.finalize(stop.without_tail(), caller.clone()))
            },
            report = &mut work => {
                let mut report = report;
                let accepted = reporter.publish(&report).await;
                if accepted { report.notices.clear(); }
                let stop = if accepted { stop } else { stop.without_tail() };
                (report, source.finalize(stop, caller).await)
            },
        };
        append_finalization(&mut report, finalization);
        report
    })
}
async fn join_preparation<T>(
    work: impl std::future::Future<Output = T>,
    caller: &CancellationToken,
    source: &CancellationToken,
) -> T {
    tokio::pin!(work);
    tokio::select! { biased;
        () = caller.cancelled() => {
            source.cancel();
            work.await
        },
        result = &mut work => result,
    }
}
fn refusal(error: InvocationError, policy: &wes_core::flow::FlowPolicy) -> ExecutionReport {
    match error {
        InvocationError::Failed(error) => Outcome::Failed(error.with_policy(policy)),
        InvocationError::Cancelled => Outcome::Cancelled(RuntimeCode::Cancelled.error(
            "Required auxiliary source admission was cancelled; the business call was not invoked.", None,
        ).with_policy(policy)),
    }.into()
}
fn protect_refusal(
    mut report: ExecutionReport,
    policy: &wes_core::flow::FlowPolicy,
) -> ExecutionReport {
    report.outcome = report.outcome.with_policy(policy);
    report.notices = report
        .notices
        .into_iter()
        .map(|error| error.with_policy(policy))
        .collect();
    report
}
fn append_finalization(report: &mut ExecutionReport, finalization: SourceFinalization) {
    let message = if !finalization.joined {
        Some(
            "The auxiliary source owner failed during physical join; coverage is incomplete. The business result was preserved.",
        )
    } else if finalization.control.as_ref().is_some_and(Result::is_err) {
        Some(
            "Auxiliary stop was refused or its outcome is unknown; cancellation and physical join completed. No control was retried. The business result was preserved.",
        )
    } else if finalization.graceful_expired {
        Some(
            "Auxiliary graceful closure expired; cancellation and physical join completed. The business result was preserved.",
        )
    } else if matches!(finalization.phase, Phase::Failed(_) | Phase::Cancelled) {
        Some(
            "Auxiliary capture ended with incomplete coverage; its physical owner joined. The business result was preserved.",
        )
    } else {
        None
    };
    if let Some(message) = message {
        report.notices.push(
            RuntimeCode::ExecutionFailed
                .error(message, None)
                .with_policy(&finalization.policy),
        );
    }
}
