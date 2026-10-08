//! FIFO admission shared by finite display, stored-value and interaction reads.
//!
//! Immediate refusal lets synchronized polling lanes repeatedly lose to the same two encoders.
//! Wait briefly for a slot instead. HTTP/1 connections already bound the waiter count to 32;
//! the deadline and shutdown cancellation bound waiting for readers/encoders. Release permits
//! when bounded encoding finishes: response bodies must never own this admission credit.
use super::*;
use tokio::sync::OwnedSemaphorePermit;

const WAIT: Duration = Duration::from_millis(500);

pub(super) async fn acquire(
    reads: &Arc<Semaphore>,
    stopped: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ()> {
    tokio::select! {
        biased;
        _ = stopped.cancelled() => Err(()),
        result = tokio::time::timeout(WAIT, reads.clone().acquire_owned()) => {
            result.map_err(|_| ())?.map_err(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::poll;
    use std::task::Poll;

    #[tokio::test(start_paused = true)]
    async fn waiters_are_fifo_without_increasing_active_read_capacity() {
        let reads = Arc::new(Semaphore::new(2));
        let stopped = CancellationToken::new();
        let first = acquire(&reads, &stopped).await.unwrap();
        let second = acquire(&reads, &stopped).await.unwrap();
        let oldest = acquire(&reads, &stopped);
        let newer = acquire(&reads, &stopped);
        tokio::pin!(oldest, newer);
        assert!(poll!(&mut oldest).is_pending());
        assert!(poll!(&mut newer).is_pending());
        drop(first);
        assert!(
            poll!(&mut newer).is_pending(),
            "new polls cannot steal a queued slot"
        );
        let admitted = oldest.await.unwrap();
        assert_eq!(reads.available_permits(), 0);
        drop(second);
        let next = newer.await.unwrap();
        assert_eq!(reads.available_permits(), 0);
        drop((admitted, next));
        assert_eq!(reads.available_permits(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_disconnect_and_shutdown_release_waiters() {
        let reads = Arc::new(Semaphore::new(1));
        let stopped = CancellationToken::new();
        let held = acquire(&reads, &stopped).await.unwrap();
        {
            let disconnected = acquire(&reads, &stopped);
            tokio::pin!(disconnected);
            assert!(poll!(&mut disconnected).is_pending());
        }
        let expired = acquire(&reads, &stopped);
        tokio::pin!(expired);
        assert!(poll!(&mut expired).is_pending());
        tokio::time::advance(WAIT).await;
        assert!(expired.await.is_err());
        let shutdown = acquire(&reads, &stopped);
        tokio::pin!(shutdown);
        assert!(poll!(&mut shutdown).is_pending());
        stopped.cancel();
        assert!(matches!(poll!(&mut shutdown), Poll::Ready(Err(()))));
        drop(held);
        assert_eq!(reads.available_permits(), 1);
        assert!(acquire(&reads, &stopped).await.is_err());
    }
}
