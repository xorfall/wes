//! Acknowledged values from a joined non-source owner. Never a progress or event queue.
use crate::{
    runtime::{Effect, Run, Runtime},
    tasks::BoundTask,
    workspace::Workspace,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use wes_core::{Value, flow::FlowPolicy};

pub(crate) struct Update {
    run: Run,
    value: Value,
    reply: oneshot::Sender<bool>,
    // Credit survives queuing and coordinator admission, including a lost reply.
    _credit: OwnedSemaphorePermit,
}
impl Update {
    pub fn apply<T: Clone>(self, runtime: &mut Runtime<T>, now: Duration) -> Vec<Effect<T>> {
        if self.reply.is_closed() {
            return vec![];
        }
        let effects = runtime.lifetime_value(&self.run, self.value, now);
        let _ = self.reply.send(
            effects.is_some()
                && runtime.value_of(self.run.node()).is_some()
                && runtime.run_of(self.run.node()) == Some(self.run.id()),
        );
        effects.unwrap_or_default()
    }
    pub fn apply_workspace(
        self,
        workspace: &mut Workspace,
        now: Duration,
    ) -> Vec<Effect<BoundTask>> {
        if self.reply.is_closed() {
            return vec![];
        }
        let effects = workspace.lifetime_value(&self.run, self.value, now);
        let _ = self.reply.send(
            effects.is_some()
                && workspace.runtime().value_of(self.run.node()).is_some()
                && workspace.runtime().run_of(self.run.node()) == Some(self.run.id()),
        );
        effects.unwrap_or_default()
    }
}

#[derive(Clone, Default)]
pub struct Reporter {
    target: Option<(Run, mpsc::Sender<Update>, Arc<Semaphore>)>,
    policy: FlowPolicy,
}
impl Reporter {
    pub(crate) fn new(run: Run, sender: mpsc::Sender<Update>, credit: Arc<Semaphore>) -> Self {
        Self {
            target: Some((run, sender, credit)),
            policy: Default::default(),
        }
    }
    pub fn silent() -> Self {
        Self::default()
    }
    pub fn with_policy(mut self, policy: &FlowPolicy) -> Self {
        self.policy = self.policy.join(policy);
        self
    }
    /// Refuse capacity before enqueueing. A lost acknowledgement is never retried;
    /// the caller still owns and joins the physical lifetime on every path.
    pub async fn publish(&self, value: Value) -> bool {
        let Some((run, sender, pool)) = &self.target else {
            return false;
        };
        let value = value.with_provenance(value.provenance().clone().with_policy(&self.policy));
        let Some(charge) = crate::value_size::value_charge(
            &value,
            wes_budgets::get("execution.publication.value.bytes"),
        )
        .and_then(|n| n.checked_add(256))
        .and_then(|n| u32::try_from(n).ok()) else {
            return false;
        };
        let Ok(credit) = pool.clone().try_acquire_many_owned(charge) else {
            return false;
        };
        let (reply, receive) = oneshot::channel();
        sender
            .send(Update {
                run: run.clone(),
                value,
                reply,
                _credit: credit,
            })
            .await
            .is_ok()
            && receive.await.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{ExecutionTraits, Outcome};
    fn value() -> Value {
        Value::new(
            wes_core::Shape::Primitive(wes_core::Primitive::Int),
            wes_core::Data::Int(1),
            Default::default(),
        )
        .unwrap()
    }
    fn entered() -> (Runtime<()>, Run) {
        let mut runtime = Runtime::new();
        runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: false,
                    bounded: true,
                },
            )
            .unwrap();
        let run = runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        assert!(runtime.enter_lifetime(&run));
        (runtime, run)
    }
    #[tokio::test]
    async fn queued_value_holds_aggregate_credit_until_guarded_coordinator_acknowledgement() {
        let (mut runtime, run) = entered();
        let pool = Arc::new(Semaphore::new(65536));
        let (sender, mut queue) = mpsc::channel(1);
        let reporter = Reporter::new(run.clone(), sender, pool.clone());
        let publish = tokio::spawn(async move { reporter.publish(value()).await });
        let update = queue.recv().await.unwrap();
        assert!(pool.available_permits() < 65536);
        assert!(!publish.is_finished());
        update.apply(&mut runtime, Duration::ZERO);
        assert!(publish.await.unwrap());
        assert_eq!(pool.available_permits(), 65536);
        assert!(runtime.is_executing(run.node()));
        assert!(!runtime.is_streaming(run.node()));
        assert_eq!(runtime.value_of(run.node()), Some(&value()));
        runtime.complete(&run, Outcome::Produced(value()), Duration::ZERO);
    }
    #[tokio::test]
    async fn exhausted_credit_and_abandoned_acknowledgement_never_publish_or_replay() {
        let (mut runtime, run) = entered();
        let (sender, mut queue) = mpsc::channel(1);
        let tiny = Reporter::new(run.clone(), sender.clone(), Arc::new(Semaphore::new(1)));
        assert!(!tiny.publish(value()).await);
        assert!(queue.try_recv().is_err());
        let pool = Arc::new(Semaphore::new(65536));
        let reporter = Reporter::new(run.clone(), sender, pool.clone());
        let publish = tokio::spawn(async move { reporter.publish(value()).await });
        let update = queue.recv().await.unwrap();
        publish.abort();
        let _ = publish.await;
        assert!(
            pool.available_permits() < 65536,
            "queued ownership outlives the requester"
        );
        assert!(update.apply(&mut runtime, Duration::ZERO).is_empty());
        assert!(runtime.value_of(run.node()).is_none());
        assert_eq!(pool.available_permits(), 65536);
        runtime.cancel(run.node(), Duration::ZERO);
        runtime.complete(&run, Outcome::Produced(value()), Duration::ZERO);
        assert!(runtime.is_drained());
    }
}
