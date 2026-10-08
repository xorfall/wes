//! Optional typed delivery branches. They never own source cancellation.
use super::{Phase, StreamError, delivery};
use crate::value_size::value_charge;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use wes_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Natural,
    Manual,
    Cancelled,
    SourceFailed,
    Rejected,
    Overloaded,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub first: Option<u64>,
    pub accepted_through: u64,
    pub end: Option<End>,
}
pub(crate) struct Item {
    pub sequence: u64,
    pub value: Value,
    pub charge: u64,
    // Both reservations survive queue removal until the writer's acknowledged/joined work ends.
    pub _ingress: delivery::Credit,
    _event: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}
struct State {
    sender: Option<mpsc::Sender<Item>>,
    status: Status,
}
pub(crate) struct Branch {
    state: Mutex<State>,
    status: watch::Sender<Status>,
    events: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    byte_limit: u64,
}
pub(crate) struct Receiver {
    pub branch: Arc<Branch>,
    receiver: mpsc::Receiver<Item>,
    pub status: watch::Receiver<Status>,
}
impl Branch {
    pub fn is_accepting(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.sender.is_some())
    }
    pub fn prepare() -> Result<Receiver, StreamError> {
        let count = wes_budgets::get("dataset.writer.pending.events") as usize;
        let bytes = wes_budgets::get("dataset.writer.pending.bytes");
        // Optional branches cannot consume the whole source's count or byte allowance.
        if count == 0
            || count.saturating_mul(4) >= delivery::events()
            || bytes == 0
            || bytes.saturating_mul(4) >= delivery::byte_credit() as u64
        {
            return Err(StreamError::Invalid);
        }
        let (sender, receiver) = mpsc::channel(count);
        let (status, updates) = watch::channel(Status::default());
        let branch = Arc::new(Self {
            state: Mutex::new(State {
                sender: Some(sender),
                status: Status::default(),
            }),
            status,
            events: Arc::new(Semaphore::new(count)),
            bytes: Arc::new(Semaphore::new(bytes as usize)),
            byte_limit: bytes,
        });
        Ok(Receiver {
            branch,
            receiver,
            status: updates,
        })
    }
    pub fn attach(&self, next: u64) -> Result<(), StreamError> {
        let mut state = self.state.lock().map_err(|_| StreamError::Closed)?;
        if state.status.first.is_some() || state.status.end.is_some() || next == 0 {
            return Err(StreamError::Invalid);
        }
        state.status.first = Some(next);
        state.status.accepted_through = next - 1;
        self.status.send_replace(state.status.clone());
        Ok(())
    }
    /// Called under the source admission lock, so attachment/stop boundaries are exact.
    pub fn deliver(&self, sequence: u64, value: &Value, ingress: &delivery::Credit) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.sender.is_none() || state.status.first.is_none() {
            return;
        }
        let item = (|| {
            let charge = value_charge(value, self.byte_limit).ok_or(StreamError::Capacity)?;
            let event = self
                .events
                .clone()
                .try_acquire_owned()
                .map_err(|_| StreamError::Capacity)?;
            let bytes = self
                .bytes
                .clone()
                .try_acquire_many_owned(charge as u32)
                .map_err(|_| StreamError::Capacity)?;
            Ok::<_, StreamError>(Item {
                sequence,
                value: value.clone(),
                charge,
                _ingress: ingress.clone(),
                _event: event,
                _bytes: bytes,
            })
        })();
        if let Ok(item) = item {
            if state.sender.as_ref().unwrap().try_send(item).is_ok() {
                state.status.accepted_through = sequence;
                self.status.send_replace(state.status.clone());
                return;
            }
        }
        state.sender = None;
        state.status.end = Some(End::Overloaded);
        self.status.send_replace(state.status.clone());
    }
    pub fn close(&self, end: End) {
        if let Ok(mut state) = self.state.lock() {
            if state.status.end.is_none() {
                state.sender = None;
                state.status.end = Some(end);
                self.status.send_replace(state.status.clone());
            }
        }
    }
    pub fn finish(&self, phase: &Phase) {
        self.close(match phase {
            Phase::Ended => End::Natural,
            Phase::Cancelled => End::Cancelled,
            _ => End::SourceFailed,
        });
    }
}
impl Receiver {
    pub async fn recv(&mut self) -> Option<Item> {
        self.receiver.recv().await
    }
    pub fn take_ready(&mut self, first: Item) -> Vec<Item> {
        let mut items = vec![first];
        while items.len() < wes_budgets::get("dataset.writer.pending.events") as usize {
            match self.receiver.try_recv() {
                Ok(item) => items.push(item),
                Err(_) => break,
            }
        }
        items
    }
    /// Dropping queued work joins no physical commit. The writer must join its current job first.
    pub fn discard_pending(&mut self) {
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.branch.close(End::Manual);
    }
}
