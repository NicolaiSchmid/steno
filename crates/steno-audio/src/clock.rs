//! The clock the session's rebuild and the meeting detector sleep on, so
//! tests drive a [`testing::ManualClock`](crate::testing::ManualClock) and
//! never wait on wall time. Swift used `any Clock<Duration>`; Rust has no
//! injectable clock in std, so this is the two-method trait both need: a
//! stopwatch read and a cancellable sleep. `steno-core` injects no clock,
//! so the `Arc<dyn Clock>` pattern is this crate's own.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Raised by whoever wants a sleeper to give up (the session's `stop()`
/// while a rebuild waits out a backoff). Shared by cloning.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    inner: Arc<CancelInner>,
}

#[derive(Debug, Default)]
struct CancelInner {
    flag: AtomicBool,
    lock: Mutex<()>,
    condvar: Condvar,
}

impl Cancel {
    /// Not yet cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises the flag and wakes every sleeper.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::Release);
        let _guard = self
            .inner
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.inner.condvar.notify_all();
    }

    /// Whether `cancel` has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::Acquire)
    }

    /// Blocks for `duration` or until cancelled; `true` when the time
    /// passed uncancelled. What [`SystemClock::sleep`] does.
    pub fn sleep(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut guard = self
            .inner
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if self.is_cancelled() {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            guard = self
                .inner
                .condvar
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// What the session and the detector need from time; see the module doc.
pub trait Clock: Send + Sync {
    /// Time since an origin of the clock's choosing; only differences are
    /// used.
    fn now(&self) -> Duration;

    /// Sleeps for `duration`; returns `false` when `cancel` was raised
    /// before the deadline.
    fn sleep(&self, duration: Duration, cancel: &Cancel) -> bool;
}

/// The wall clock.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl SystemClock {
    /// Origin now.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration, cancel: &Cancel) -> bool {
        cancel.sleep(duration)
    }
}
