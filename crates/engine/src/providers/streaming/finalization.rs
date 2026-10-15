//! One explicit typed stop, followed by physical source/controller join on every path.
use super::*;
use std::time::Duration;
use streams::{ControlContracts, ControlError, LiveControls, Phase, ReadyReceipt};

#[cfg(test)]
mod tests;

/// These bound waiting and graceful closure, not the actual physical join obligation.
#[derive(Clone, Copy, Debug)]
pub struct FinalizationLimits {
    pub tail: Duration,
    pub control: Duration,
    pub graceful: Duration,
}
impl Default for FinalizationLimits {
    fn default() -> Self {
        Self {
            tail: Duration::ZERO,
            control: Duration::from_secs(5),
            graceful: Duration::from_secs(5),
        }
    }
}
impl FinalizationLimits {
    fn valid(self) -> bool {
        self.tail <= streams::MAX_READY_WAIT
            && !self.control.is_zero()
            && self.control <= streams::MAX_READY_WAIT
            && !self.graceful.is_zero()
            && self.graceful <= streams::MAX_READY_WAIT
    }
}
/// A validated declaration, not a live port. Private identity binds the original preparation.
pub struct PreparedStop {
    identity: Arc<()>,
    run: Run,
    contracts: ControlContracts,
    action: Value,
    limits: FinalizationLimits,
}
/// One weak original ready connection. Never reconstruct this from stored descriptors.
pub struct BoundStop {
    fence: Arc<SourceFence>,
    ready: ReadyReceipt,
    port: LiveControls,
    action: Value,
    limits: FinalizationLimits,
}
impl BoundStop {
    /// A refused business admission has no reason to wait its post-call tail.
    pub fn without_tail(mut self) -> Self {
        self.limits.tail = Duration::ZERO;
        self
    }
    fn current(&self) -> bool {
        // Never call into the source from this local admission callback.
        self.ready
            .admit(self.ready.run(), || self.fence.check().is_ok())
            .is_ok_and(|allowed| allowed)
    }
}
#[derive(Clone, Debug)]
pub struct SourceFinalization {
    /// Metadata inherits source, action and confirmed reply restrictions without exposing payloads.
    pub policy: wes_core::flow::FlowPolicy,
    /// None if already terminal. A reply confirms only the typed action, not physical closure.
    pub control: Option<Result<(), ControlError>>,
    pub cancellation_requested: bool,
    pub graceful_expired: bool,
    pub phase: Phase,
    pub joined: bool,
}
impl PreparedSource {
    /// Validate static stop semantics and bounds before any source subscription starts.
    pub fn prepare_stop(
        &self,
        action: Value,
        limits: FinalizationLimits,
    ) -> Result<PreparedStop, InvocationError> {
        let refused = || stop_refusal(self.attribution.policy().join(action.provenance().policy()));
        if !limits.valid()
            || self.cancellation.is_cancelled()
            || !self.bound.context.authority.is_local_user()
            || !action.data().is_inline()
            || crate::value_size::value_charge(&action, 64 * 1024).is_none()
        {
            return Err(refused());
        }
        let contracts = self
            .bound
            .provider
            .streams
            .as_ref()
            .and_then(|streams| streams.control_contracts(&self.bound.invocation.capability))
            .ok_or_else(refused)?;
        let action = boundary::literal(
            "action",
            std::slice::from_ref(&contracts.input),
            &contracts.input.shape(),
            &action,
        )
        .map_err(|_| refused())?;
        self.bound
            .require_dispatch(
                &self
                    .carried
                    .clone()
                    .with_policy(action.provenance().policy()),
            )
            .map_err(|_| refused())?;
        Ok(PreparedStop {
            identity: self.identity.clone(),
            run: self.run.clone(),
            contracts,
            action,
            limits,
        })
    }
}
impl OwnedSource {
    /// Bind before business dispatch, against the same original readiness proof and static contracts.
    pub fn bind_stop(
        &self,
        plan: PreparedStop,
        prerequisite: &SourcePrerequisite,
    ) -> Result<BoundStop, InvocationError> {
        let refused = || stop_refusal(self.fence.policy.join(plan.action.provenance().policy()));
        if !Arc::ptr_eq(&self.fence.identity, &plan.identity)
            || !Arc::ptr_eq(&self.fence, &prerequisite.fence)
            || plan.run != *prerequisite.ready.run()
        {
            return Err(refused());
        }
        prerequisite.check()?;
        let port = self
            .handle
            .live_controls(&plan.run, self.fence.bound.context.authority.clone())
            .map_err(|_| refused())?;
        let (_, epoch, input, output) = port.description().map_err(|_| refused())?;
        if epoch != prerequisite.ready.connection_epoch()
            || input.digest() != plan.contracts.input.digest()
            || output.digest() != plan.contracts.output.digest()
        {
            return Err(refused());
        }
        prerequisite.check()?;
        Ok(BoundStop {
            fence: self.fence.clone(),
            ready: prerequisite.ready.clone(),
            port,
            action: plan.action,
            limits: plan.limits,
        })
    }
    /// Caller cancellation skips the tail. Cleanup itself remains independently owned and joined.
    /// A control timeout is uncertainty, never permission to retry or detach its physical future.
    pub async fn finalize(self, stop: BoundStop, caller: CancellationToken) -> SourceFinalization {
        let mut policy = self.fence.policy.join(stop.action.provenance().policy());
        let correct_owner = Arc::ptr_eq(&self.fence, &stop.fence);
        let handle = self.handle;
        let mut changes = handle.subscribe();
        let source_cancellation = handle.cancellation_token();
        let until = tokio::time::Instant::now() + stop.limits.tail;
        while correct_owner
            && !caller.is_cancelled()
            && stop.current()
            && tokio::time::Instant::now() < until
        {
            tokio::select! { biased;
                () = caller.cancelled() => break,
                () = source_cancellation.cancelled() => break,
                change = changes.changed() => if change.is_err() { break; },
                _ = tokio::time::sleep_until(until) => break,
            }
        }
        let terminal = matches!(
            handle.snapshot().phase,
            Phase::Ended | Phase::Cancelled | Phase::Failed(_)
        );
        let control = if !correct_owner {
            Some(Err(ControlError::Changed))
        } else if terminal {
            None
        } else if !stop.current()
            || stop
                .fence
                .bound
                .require_dispatch(
                    &stop
                        .fence
                        .carried
                        .clone()
                        .with_policy(stop.action.provenance().policy()),
                )
                .is_err()
        {
            Some(Err(ControlError::Changed))
        } else {
            // The business caller may be cancelled; its requested cleanup is still explicit.
            // Do not abandon a stop just because that caller's token is already set.
            Some(
                match stop
                    .port
                    .invoke(stop.action, stop.limits.control, CancellationToken::new())
                    .await
                {
                    Ok(value) => {
                        policy = policy.join(value.provenance().policy());
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
            )
        };
        let mut cancellation_requested = control.as_ref().is_some_and(Result::is_err);
        let mut graceful_expired = false;
        let joined = self.task.join();
        tokio::pin!(joined);
        let result = if cancellation_requested {
            handle.cancel();
            joined.await
        } else {
            tokio::select! { biased;
                result = &mut joined => result,
                _ = tokio::time::sleep(stop.limits.graceful) => {
                    cancellation_requested = true;
                    graceful_expired = true;
                    handle.cancel();
                    joined.await
                },
            }
        };
        let phase = match &handle.snapshot().phase {
            Phase::Failed(error) => Phase::Failed(error.clone().with_policy(&policy)),
            phase => phase.clone(),
        };
        SourceFinalization {
            policy,
            control,
            cancellation_requested,
            graceful_expired,
            phase,
            joined: result.is_ok(),
        }
    }
}
fn stop_refusal(policy: wes_core::flow::FlowPolicy) -> InvocationError {
    InvocationError::Failed(RuntimeCode::ExecutionFailed.error("Source stop requires valid static typed controls, native authority and the original ready owner; nothing was dispatched.", None).with_policy(&policy))
}
