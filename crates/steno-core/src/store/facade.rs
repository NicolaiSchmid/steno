//! How async code reaches the store (feature `tokio`): one store step
//! through [`Store::reading`] or [`Store::writing`] on tokio's blocking
//! pool, so a step that waits on SQLite's busy timeout never holds a
//! runtime worker. Callers on threads of their own call the store directly.

use std::sync::Arc;

use super::{Result, Store, StoreError};

impl Store {
    /// Runs `body`, a step that only reads, on tokio's blocking pool and
    /// returns its value. One call per logical step, not per method. A
    /// panic in `body` resumes in the caller; a dropped future does not
    /// cancel the step.
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
        self.on_the_blocking_pool(body).await
    }

    /// Runs `body`, a step that writes, as [`Store::reading`] does: the
    /// step's reads and its write in one body, so nothing of the caller's
    /// runs between them.
    pub async fn writing<T: Send + 'static>(
        self: &Arc<Self>,
        body: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.on_the_blocking_pool(body).await
    }

    /// `body` on the blocking pool. A runtime that shuts down first cancels
    /// it, which surfaces as an I/O error.
    async fn on_the_blocking_pool<T: Send + 'static>(
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use crate::Store;
    use crate::store::StoreError;
    use crate::testing::WriteLockHold;

    /// A write waiting out another connection's lock waits on the blocking
    /// pool, so another task on the runtime's one worker still runs.
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

    /// A step the pool never ran because its runtime shut down is an I/O
    /// error, and its body never runs.
    #[test]
    fn a_step_the_pool_never_ran_is_an_io_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        runtime.shutdown_background();
        let store = Arc::new(Store::in_memory().unwrap());
        let ran = Arc::new(AtomicBool::new(false));
        let running = ran.clone();
        let outcome = handle.block_on(store.writing(move |_| {
            running.store(true, Ordering::SeqCst);
            Ok(())
        }));
        assert!(
            matches!(outcome, Err(StoreError::Io(_))),
            "cancelled: {outcome:?}"
        );
        assert!(!ran.load(Ordering::SeqCst), "the body did not run");
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
