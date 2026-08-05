//! WorkerPool — concurrent agent execution with semaphore limiting.
//! See AGENTS.md §9 for detailed rules.
//!
//! Uses `tokio::sync::Semaphore` to bound the number of concurrently
//! executing agent tasks. The pool itself does not manage task lifecycle
//! beyond acquiring/releasing permits.

use std::sync::Arc;

use crate::infra::error::TaijiError;

/// Bounded concurrency pool for agent execution.
///
/// Every call to [`execute`](WorkerPool::execute) acquires a semaphore permit
/// before running the future. When the semaphore has no remaining permits,
/// the caller will asynchronously wait until one becomes available.
///
/// The [`acquire`](WorkerPool::acquire) method returns an owned permit suitable
/// for long-running or `tokio::spawn`-based tasks — the caller holds the permit
/// for the lifetime of the spawned work.
///
/// # Example
///
/// ```ignore
/// let pool = WorkerPool::new(4);
/// let result = pool.execute(async { 42 }).await.unwrap();
/// assert_eq!(result, 42);
/// ```
pub struct WorkerPool {
    semaphore: Arc<tokio::sync::Semaphore>,
    max_concurrent: usize,
}

impl WorkerPool {
    /// Create a new pool limiting concurrency to `max_concurrent`.
    ///
    /// # Panics
    ///
    /// Panics if `max_concurrent` is 0.
    pub fn new(max_concurrent: usize) -> Self {
        assert!(
            max_concurrent > 0,
            "WorkerPool::new requires max_concurrent > 0, got {max_concurrent}",
        );
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            max_concurrent,
        }
    }

    /// Execute a future with a semaphore permit held.
    ///
    /// Acquires a permit from the semaphore (waiting if necessary), runs `f`
    /// inside the permit's scope, and releases the permit when `f` completes.
    ///
    /// If the semaphore has been closed (e.g. all permits have been forgotten
    /// — an unusual edge case), this method returns
    /// [`TaijiError::WorkerPoolUnavailable`] instead of panicking, so the
    /// caller can degrade gracefully (e.g. abort sibling tasks and propagate
    /// the error) rather than crashing the whole process.
    pub async fn execute<F, T>(&self, f: F) -> Result<T, TaijiError>
    where
        F: std::future::Future<Output = T> + Send,
        T: Send,
    {
        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| TaijiError::WorkerPoolUnavailable {
                context: e.to_string(),
            })?;

        // DropGuards guarantee release even if f panics.
        // The permit is held until f completes.
        let result = f.await;

        // Permit is dropped here, automatically returning it to the semaphore.
        drop(permit);

        Ok(result)
    }

    /// Acquire an owned permit from the semaphore.
    ///
    /// This is useful when you want to hold the permit across a
    /// `tokio::spawn` boundary — the permit is returned as an owned value
    /// that can be moved into the spawned task.
    ///
    /// If the semaphore has been closed (all permits permanently lost),
    /// this returns [`TaijiError::WorkerPoolUnavailable`] (same as
    /// [`execute`](WorkerPool::execute)) instead of panicking.
    pub async fn acquire(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, TaijiError> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| TaijiError::WorkerPoolUnavailable {
                context: e.to_string(),
            })
    }

    /// Maximum number of concurrent executions allowed.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Number of permits currently available (not yet acquired).
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

impl Clone for WorkerPool {
    /// Cloning a WorkerPool shares the same underlying semaphore
    /// (via Arc), so concurrency limits apply across all clones.
    fn clone(&self) -> Self {
        Self {
            semaphore: Arc::clone(&self.semaphore),
            max_concurrent: self.max_concurrent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_execute_returns_value() {
        let pool = WorkerPool::new(2);
        let result = pool.execute(async { 42 }).await.unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_available_permits() {
        let pool = WorkerPool::new(4);
        assert_eq!(pool.available_permits(), 4);
        assert_eq!(pool.max_concurrent(), 4);

        // Acquire one permit.
        let _permit = pool.semaphore.acquire().await.unwrap();
        assert_eq!(pool.available_permits(), 3);
    }

    #[tokio::test]
    async fn test_max_concurrent_enforced() {
        let pool = Arc::new(WorkerPool::new(2));
        let counter = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = Arc::clone(&pool);
            let counter = Arc::clone(&counter);
            let max_seen = Arc::clone(&max_seen);
            handles.push(tokio::spawn(async move {
                pool.execute(async move {
                    let prev = counter.fetch_add(1, Ordering::SeqCst);
                    max_seen.fetch_max(prev + 1, Ordering::SeqCst);
                    // Simulate work.
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    counter.fetch_sub(1, Ordering::SeqCst);
                })
                .await
                .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // With 2 permits, the maximum concurrent executions should be at most 2.
        let seen = max_seen.load(Ordering::SeqCst);
        assert!(seen <= 2, "expected max concurrency <= 2, got {seen}");
    }

    #[tokio::test]
    async fn test_max_concurrent_method() {
        let pool = WorkerPool::new(10);
        assert_eq!(pool.max_concurrent(), 10);
    }

    #[tokio::test]
    async fn test_available_permits_after_clone() {
        let pool = WorkerPool::new(3);
        let pool2 = pool.clone();

        // Both share the same semaphore.
        let _permit = pool.semaphore.acquire().await.unwrap();
        assert_eq!(pool2.available_permits(), 2);
    }

    #[test]
    #[should_panic(expected = "max_concurrent > 0")]
    fn test_zero_max_concurrent_panics() {
        WorkerPool::new(0);
    }

    #[tokio::test]
    async fn test_closed_semaphore_acquire_returns_error() {
        // Closing the semaphore (all permits permanently lost) must yield a
        // `WorkerPoolUnavailable` error, NOT a panic — the whole serve/run
        // process must survive this edge case.
        let pool = WorkerPool::new(2);
        pool.semaphore.close();

        let err = pool.acquire().await.unwrap_err();
        assert!(
            matches!(err, TaijiError::WorkerPoolUnavailable { .. }),
            "expected WorkerPoolUnavailable, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_closed_semaphore_execute_returns_error() {
        let pool = WorkerPool::new(2);
        pool.semaphore.close();

        let err = pool.execute(async { 42 }).await.unwrap_err();
        assert!(
            matches!(err, TaijiError::WorkerPoolUnavailable { .. }),
            "expected WorkerPoolUnavailable, got {err:?}"
        );
    }
}
