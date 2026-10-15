//! How async code reaches the store (feature `tokio`): one store step
//! through [`Store::reading`] or [`Store::writing`], run on tokio's blocking
//! pool, so a step that waits on SQLite (another process holding the write
//! lock, up to the busy timeout) never holds a runtime worker, which the
//! pipeline's runs, the handover and the timers share. Synchronous callers
//! on threads of their own (the recorder's start and stop, the CLI's
//! commands) keep calling the store directly. Decision 1 of
//! `.plans/2026-10-12-store-off-the-workers.md`. Rust only: the Swift
//! store's `async` calls ran on GRDB's own queues.

use std::sync::Arc;

use super::{Result, Store, StoreError};

impl Store {
    /// Runs `body`, a step that only reads, on tokio's blocking pool with
    /// this handle, and returns its value. One call per logical step, not
    /// per method: a step's reads go in one body. A panic in `body` resumes
    /// in the caller, as it would have inline. The body owns what it takes,
    /// so the step runs on whichever thread the pool gives it, and a dropped
    /// future does not cancel it: the step still runs to its end.
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> steno_core::store::Result<()> {
    /// use std::sync::Arc;
    /// use steno_core::Store;
    ///
    /// let store = Arc::new(Store::in_memory()?);
    /// let settings = store.reading(|store| store.settings()).await?;
    /// assert!(settings.meeting_detection_enabled);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn reading<T: Send + 'static>(
        self: &Arc<Self>,
        body: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.off_the_workers(body).await
    }

    /// Runs `body`, a step that writes, on tokio's blocking pool with this
    /// handle, and returns its value: the step's reads and its write in one
    /// body, so nothing else of the caller's runs between them. The rest is
    /// as [`Store::reading`]. Kept apart from it so a step that writes can
    /// move to a writer thread of its own without touching a caller.
    pub async fn writing<T: Send + 'static>(
        self: &Arc<Self>,
        body: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.off_the_workers(body).await
    }

    /// `body` on the blocking pool. A runtime that shuts down before the
    /// pool ran it cancels it, which surfaces as an I/O error.
    async fn off_the_workers<T: Send + 'static>(
        self: &Arc<Self>,
        body: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = Arc::clone(self);
        match tokio::task::spawn_blocking(move || body(&store)).await {
            Ok(outcome) => outcome,
            Err(join) => match join.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(cancelled) => Err(StoreError::Io(std::io::Error::other(cancelled))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::Store;
    use crate::testing::WriteLockHold;

    /// A write that waits out another connection's lock waits on the
    /// blocking pool, so another task on the runtime's one worker still
    /// runs, and the write commits once the lock goes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_waiting_write_leaves_the_worker_free() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("steno.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        let hold = WriteLockHold::new(&path);
        let write = tokio::spawn({
            let store = store.clone();
            async move { store.writing(Store::rebuild_search_index).await }
        });
        // The write is in its busy wait by now.
        std::thread::sleep(Duration::from_millis(100));
        for _ in 0..5 {
            let asked = Instant::now();
            tokio::spawn(async {}).await.unwrap();
            let ran = asked.elapsed();
            assert!(
                ran < Duration::from_millis(200),
                "a task waited {ran:?} for the worker"
            );
        }
        assert!(!write.is_finished(), "the write still waits");
        drop(hold);
        write.await.unwrap().unwrap();
    }

    /// A panic in the body resumes in the caller.
    #[tokio::test]
    #[should_panic(expected = "in the body")]
    async fn a_panic_in_the_body_resumes_in_the_caller() {
        let store = Arc::new(Store::in_memory().unwrap());
        let _ = store
            .reading(|_| -> crate::store::Result<()> { panic!("in the body") })
            .await;
    }
}
