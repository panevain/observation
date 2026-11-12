//! A concurrency limit with a bounded number of waiters.

use std::mem::DropGuard;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{AcquireError, Semaphore, SemaphorePermit};

/// Lets `concurrency` callers through at a time and at most `max_waiting`
/// more wait for a turn; anyone beyond that is turned away at once, so a
/// burst cannot build an unbounded backlog behind a slow upstream.
#[derive(Debug)]
pub struct BoundedQueue {
    permits: Semaphore,
    /// Callers between [`BoundedQueue::enter`] and getting a permit.
    waiting: AtomicUsize,
    max_waiting: usize,
}

/// Why [`BoundedQueue::enter`] gave no permit.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// `max_waiting` callers were already waiting.
    #[error("too many requests waiting")]
    Full,
    /// The permits were closed.
    #[error(transparent)]
    Closed(#[from] AcquireError),
}

impl BoundedQueue {
    /// `concurrency` permits; at most `max_waiting` callers may wait for one
    /// (0 refuses everyone, see [`BoundedQueue::enter`]).
    pub fn new(concurrency: usize, max_waiting: usize) -> Self {
        Self {
            permits: Semaphore::new(concurrency),
            waiting: AtomicUsize::new(0),
            max_waiting,
        }
    }

    /// Waits for a permit, or fails at once with [`QueueError::Full`] if
    /// `max_waiting` callers are already waiting. A caller counts as waiting
    /// from this call until it holds a permit or its future is dropped, even
    /// when a permit is free at once, so a `max_waiting` of 0 refuses everyone.
    pub async fn enter(&self) -> Result<SemaphorePermit<'_>, QueueError> {
        // Leaves the waiting count when dropped: on return, or if this future is dropped.
        let guard = DropGuard::new(&self.waiting, |w| {
            w.fetch_sub(1, Ordering::SeqCst);
        });
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= self.max_waiting {
            return Err(QueueError::Full);
        }
        let permit = self.permits.acquire().await?;
        drop(guard);
        Ok(permit)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::Poll;

    use super::*;

    /// Polls `f` once.
    async fn poll_once<F: Future + Unpin>(f: &mut F) -> Poll<F::Output> {
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *f).poll(cx))).await
    }

    #[tokio::test]
    async fn turns_callers_away_past_the_waiting_limit() {
        let q = BoundedQueue::new(1, 2);
        let held = q.enter().await.unwrap();
        // Two may wait for the one permit...
        let mut a = Box::pin(q.enter());
        let mut b = Box::pin(q.enter());
        assert!(poll_once(&mut a).await.is_pending());
        assert!(poll_once(&mut b).await.is_pending());
        // ...a third is refused without waiting.
        assert!(matches!(q.enter().await, Err(QueueError::Full)));
        // A waiter that gives up frees its place.
        drop(b);
        let mut c = Box::pin(q.enter());
        assert!(poll_once(&mut c).await.is_pending());
        // Releasing the permit lets a waiter in, which frees its place too.
        drop(held);
        let Poll::Ready(Ok(next)) = poll_once(&mut a).await else {
            panic!("the first waiter should get the permit");
        };
        let mut d = Box::pin(q.enter());
        assert!(poll_once(&mut d).await.is_pending());
        assert!(matches!(q.enter().await, Err(QueueError::Full)));
        drop(next);
        assert!(matches!(poll_once(&mut c).await, Poll::Ready(Ok(_))));
    }

    #[tokio::test]
    async fn concurrency_lets_several_through_without_waiting() {
        let q = BoundedQueue::new(2, 1);
        let first = q.enter().await.unwrap();
        let _second = q.enter().await.unwrap();
        let mut third = Box::pin(q.enter());
        assert!(poll_once(&mut third).await.is_pending());
        assert!(matches!(q.enter().await, Err(QueueError::Full)));
        drop(first);
        assert!(matches!(poll_once(&mut third).await, Poll::Ready(Ok(_))));
    }

    #[tokio::test]
    async fn closed_permits_are_reported() {
        let q = BoundedQueue::new(1, 1);
        q.permits.close();
        let err = q.enter().await.unwrap_err();
        assert!(matches!(err, QueueError::Closed(_)));
        // Callers map this to their own error with the semaphore's message.
        assert_eq!(err.to_string(), "semaphore closed");
        // The refused caller no longer counts as waiting.
        assert!(matches!(q.enter().await, Err(QueueError::Closed(_))));
    }
}
