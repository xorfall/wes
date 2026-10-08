//! Credits travel with an event until every ordered consumer has finished it.
//! Moving an event from ingress into the actor never creates a second buffer budget.
use super::StreamError;
use crate::driver::CancellationToken;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use wes_core::Value;

pub(crate) fn max_wait() -> std::time::Duration {
    std::time::Duration::from_millis(wes_budgets::get("stream.ingress.wait.ms"))
}

pub(crate) fn events() -> usize {
    wes_budgets::get("stream.ingress.events") as usize
}
pub(crate) fn byte_credit() -> usize {
    wes_budgets::get("stream.ingress.bytes") as usize
}

#[derive(Clone, Debug)]
pub(crate) struct Budget(Arc<Semaphore>);
impl Budget {
    pub(crate) fn is_idle(&self) -> bool {
        self.0.available_permits() == wes_budgets::get("stream.ingress.total.bytes") as usize
    }
}
impl Default for Budget {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(
            wes_budgets::get("stream.ingress.total.bytes") as usize,
        )))
    }
}
struct Permits {
    _event: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
    _total: OwnedSemaphorePermit,
}
/// One ingress reservation, retired only after every branch has joined its use.
#[derive(Clone)]
pub(crate) struct Credit {
    _permits: Arc<Permits>,
}
pub(crate) struct Event {
    pub sequence: u64,
    pub value: Value,
    pub credit: Credit,
}
pub(super) struct Sender {
    events: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    total: Budget,
    pub channel: mpsc::Sender<Event>,
}
impl Sender {
    pub fn new(total: Budget) -> (Arc<Self>, mpsc::Receiver<Event>) {
        let (channel, receiver) = mpsc::channel(events());
        (
            Arc::new(Self {
                events: Arc::new(Semaphore::new(events())),
                bytes: Arc::new(Semaphore::new(byte_credit())),
                total,
                channel,
            }),
            receiver,
        )
    }
    fn charge(value: &Value) -> Result<u32, StreamError> {
        crate::value_size::value_charge(value, byte_credit() as u64)
            .map(|n| n as u32)
            .ok_or(StreamError::Capacity)
    }
    pub fn reserve(&self, value: &Value) -> Result<Credit, StreamError> {
        let charge = Self::charge(value)?;
        Ok(Credit {
            _permits: Arc::new(Permits {
                _event: self
                    .events
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| StreamError::Capacity)?,
                _bytes: self
                    .bytes
                    .clone()
                    .try_acquire_many_owned(charge)
                    .map_err(|_| StreamError::Capacity)?,
                _total: self
                    .total
                    .0
                    .clone()
                    .try_acquire_many_owned(charge)
                    .map_err(|_| StreamError::Capacity)?,
            }),
        })
    }
    pub async fn reserve_wait(
        &self,
        value: &Value,
        token: &CancellationToken,
    ) -> Result<Credit, StreamError> {
        let charge = Self::charge(value)?;
        let acquire = async {
            Ok(Credit {
                _permits: Arc::new(Permits {
                    _event: self
                        .events
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|_| StreamError::Closed)?,
                    _bytes: self
                        .bytes
                        .clone()
                        .acquire_many_owned(charge)
                        .await
                        .map_err(|_| StreamError::Closed)?,
                    _total: self
                        .total
                        .0
                        .clone()
                        .acquire_many_owned(charge)
                        .await
                        .map_err(|_| StreamError::Closed)?,
                }),
            })
        };
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(StreamError::Closed),
            result = tokio::time::timeout(max_wait(), acquire) => result.unwrap_or(Err(StreamError::Overloaded)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Provenance, Shape};
    fn value(bytes: usize) -> Value {
        Value::new(
            Shape::Unknown,
            Data::Bytes(vec![0; bytes].into()),
            Provenance::default(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn count_credits_cover_ingress_and_consumer_and_cancellation_interrupts_wait() {
        let (sender, _receiver) = Sender::new(Budget::default());
        let value = value(1);
        let mut held = (0..events())
            .map(|_| sender.reserve(&value).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(sender.reserve(&value), Err(StreamError::Capacity)));
        let token = CancellationToken::new();
        let waiting = sender.reserve_wait(&value, &token);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut waiting)
                .await
                .is_err()
        );
        held.pop();
        let credit = waiting.await.unwrap();
        assert!(matches!(sender.reserve(&value), Err(StreamError::Capacity)));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            sender.reserve_wait(&value, &cancelled).await,
            Err(StreamError::Closed)
        ));
        drop(credit);
        drop(held);
        assert_eq!(sender.events.available_permits(), events());
        assert_eq!(sender.bytes.available_permits(), byte_credit());
    }
    #[tokio::test(start_paused = true)]
    async fn prolonged_backpressure_times_out_and_releases_all_partial_reservations() {
        let (sender, _receiver) = Sender::new(Budget::default());
        let value = value(1);
        let held = (0..events())
            .map(|_| sender.reserve(&value).unwrap())
            .collect::<Vec<_>>();
        let token = CancellationToken::new();
        let start = tokio::time::Instant::now();
        assert!(matches!(
            sender.reserve_wait(&value, &token).await,
            Err(StreamError::Overloaded)
        ));
        assert_eq!(start.elapsed(), max_wait());
        drop(held);
        assert_eq!(sender.events.available_permits(), events());
        assert_eq!(sender.bytes.available_permits(), byte_credit());
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            sender.reserve_wait(&value, &token).await,
            Err(StreamError::Closed)
        ));
        assert!(sender.total.is_idle());
    }
    #[test]
    fn aggregate_bytes_are_shared_and_every_refusal_releases_partial_reservations() {
        let budget = Budget::default();
        let large = value(1024 * 1024);
        let senders = (0..8)
            .map(|_| Sender::new(budget.clone()).0)
            .collect::<Vec<_>>();
        let mut held = vec![];
        for sender in &senders {
            for _ in 0..7 {
                if let Ok(credit) = sender.reserve(&large) {
                    held.push(credit);
                }
            }
        }
        assert!(held.len() < 32);
        assert!(matches!(
            senders[7].reserve(&large),
            Err(StreamError::Capacity)
        ));
        drop(held);
        assert_eq!(
            budget.0.available_permits(),
            wes_budgets::get("stream.ingress.total.bytes") as usize
        );
        for sender in senders {
            assert_eq!(sender.events.available_permits(), events());
            assert_eq!(sender.bytes.available_permits(), byte_credit());
        }
    }
}
