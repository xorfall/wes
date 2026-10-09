//! Bounded wait registrations. Runtime authority remains in the caller, which supplies read access.
use super::DriverError;
use crate::runtime::{OutputSelection, Runtime};
use std::future::pending;
use tokio::{sync::oneshot, time::Instant};

fn max_waiters() -> usize {
    wes_budgets::get("execution.waiters") as usize
}
fn max_total_selected_outputs() -> usize {
    wes_budgets::get("execution.selected.total") as usize
}
pub(crate) fn max_selected_outputs() -> usize {
    wes_budgets::get("execution.selected") as usize
}
pub(crate) enum Waiter {
    SourceReady {
        node: crate::graph::NodeId,
        run: crate::runtime::RunId,
        until: Instant,
        reply: oneshot::Sender<Result<bool, DriverError>>,
    },
    Idle(oneshot::Sender<Result<(), DriverError>>),
    Outputs {
        selected: OutputSelection,
        until: Instant,
        reply: oneshot::Sender<Result<bool, DriverError>>,
    },
}
impl Waiter {
    fn selections(&self) -> usize {
        match self {
            Self::Idle(_) => 0,
            Self::Outputs { selected, .. } => selected.len(),
            Self::SourceReady { .. } => 1,
        }
    }
    fn abandoned(&self) -> bool {
        match self {
            Self::Idle(reply) => reply.is_closed(),
            Self::Outputs { reply, .. } | Self::SourceReady { reply, .. } => reply.is_closed(),
        }
    }
    fn answer(self, result: Result<bool, DriverError>) {
        match self {
            Self::Idle(reply) => {
                let _ = reply.send(result.map(|_| ()));
            }
            Self::Outputs { reply, .. } | Self::SourceReady { reply, .. } => {
                let _ = reply.send(result);
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct Waiters(Vec<Waiter>);
impl Waiters {
    pub fn insert(&mut self, waiter: Waiter) {
        if waiter.abandoned() {
            return;
        }
        if waiter.selections() > max_selected_outputs() {
            waiter.answer(Err(DriverError::InvalidWait));
            return;
        }
        self.0.retain(|waiter| !waiter.abandoned());
        let selected = self.0.iter().map(Waiter::selections).sum::<usize>();
        if self.0.len() >= max_waiters()
            || waiter.selections() > max_total_selected_outputs() - selected
        {
            waiter.answer(Err(DriverError::WaitCapacity));
        } else {
            self.0.push(waiter);
        }
    }
    pub fn poll<T: Clone>(&mut self, runtime: &Runtime<T>, io_idle: bool) {
        let now = Instant::now();
        let mut index = 0;
        while index < self.0.len() {
            let waiter = &self.0[index];
            if waiter.abandoned() {
                self.0.swap_remove(index);
                continue;
            }
            let result = match waiter {
                Waiter::SourceReady {
                    node, run, until, ..
                } => {
                    if runtime.is_closed() {
                        Some(Err(DriverError::Stopped))
                    } else if runtime.run_of(node) != Some(run) {
                        Some(Err(DriverError::SourceChanged))
                    } else if runtime.is_streaming(node) {
                        Some(Ok(true))
                    } else if runtime.stream_phase(node) == "closing" || !runtime.has_lease(node) {
                        Some(Err(DriverError::SourceClosed))
                    } else if now >= *until {
                        Some(Ok(false))
                    } else {
                        None
                    }
                }
                Waiter::Idle(_) => (runtime.is_idle() && io_idle).then_some(Ok(true)),
                Waiter::Outputs {
                    selected, until, ..
                } => {
                    if runtime.is_closed() {
                        Some(Err(DriverError::Stopped))
                    } else {
                        runtime
                            .selected_outputs(selected)
                            .map(Ok)
                            .or_else(|| (now >= *until).then_some(Ok(false)))
                    }
                }
            };
            if let Some(result) = result {
                self.0.swap_remove(index).answer(result);
            } else {
                index += 1;
            }
        }
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.0
            .iter()
            .filter_map(|waiter| match waiter {
                Waiter::Outputs { until, .. } | Waiter::SourceReady { until, .. } => Some(*until),
                Waiter::Idle(_) => None,
            })
            .min()
    }
}
pub(crate) async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeId, OutputRef};
    use std::time::Duration;

    #[tokio::test]
    async fn capacity_is_shared_and_abandoned_waiters_release_their_registration() {
        let mut waiters = Waiters::default();
        let mut receivers = vec![];
        for _ in 0..max_waiters() {
            let (reply, receive) = oneshot::channel();
            waiters.insert(Waiter::Idle(reply));
            receivers.push(receive);
        }
        let (reply, receive) = oneshot::channel();
        waiters.insert(Waiter::Idle(reply));
        assert_eq!(receive.await.unwrap(), Err(DriverError::WaitCapacity));
        drop(receivers.pop());
        let (reply, receive) = oneshot::channel();
        waiters.insert(Waiter::Idle(reply));
        waiters.poll(&Runtime::<()>::new(), true);
        assert_eq!(receive.await.unwrap(), Ok(()));
        for receive in receivers {
            assert_eq!(receive.await.unwrap(), Ok(()));
        }
        assert!(waiters.0.is_empty());
    }

    #[tokio::test]
    async fn output_reference_budgets_bound_the_aggregate_not_only_each_wait() {
        let selected = OutputSelection::new(
            (0..max_selected_outputs())
                .map(|n| OutputRef::data(NodeId::new(format!("n{n}")).unwrap())),
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        let mut waiters = Waiters::default();
        let mut receivers = vec![];
        for _ in 0..(max_total_selected_outputs() / max_selected_outputs()) {
            let (reply, receive) = oneshot::channel();
            waiters.insert(Waiter::Outputs {
                selected: selected.clone(),
                until,
                reply,
            });
            receivers.push(receive);
        }
        let (reply, receive) = oneshot::channel();
        waiters.insert(Waiter::Outputs {
            selected: selected.clone(),
            until,
            reply,
        });
        assert_eq!(receive.await.unwrap(), Err(DriverError::WaitCapacity));
        let too_large = OutputSelection::new(
            selected
                .outputs()
                .chain([OutputRef::data(NodeId::new("extra").unwrap())]),
        )
        .unwrap();
        let (reply, receive) = oneshot::channel();
        waiters.insert(Waiter::Outputs {
            selected: too_large,
            until,
            reply,
        });
        assert_eq!(receive.await.unwrap(), Err(DriverError::InvalidWait));
        drop(receivers);
        waiters.poll(&Runtime::<()>::new(), true);
        assert!(waiters.0.is_empty());
        assert!(waiters.next_deadline().is_none());
    }
}
