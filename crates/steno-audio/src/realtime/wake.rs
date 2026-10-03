//! The consumer wake-up behind the rings: a counting signal the producer
//! raises without a lock and the consumer waits on with a timeout. Swift
//! uses a `DispatchSemaphore`; this is the same shape over a futex-free
//! `Mutex<u32>` and `Condvar`, taken only on the consumer side and in
//! `signal` when a waiter is actually parked.
//!
//! The producer path (`signal`) is an atomic increment plus, when the
//! consumer is parked, one `notify_one`. `notify_one` is the one call on
//! the real-time path that is not a plain atomic; it takes no lock in the
//! std implementation on macOS and Linux (a futex or `ulock` wake syscall),
//! and the IOProc in Swift pays the same `DispatchSemaphore.signal` cost.
//!
//! The hand-off is the store/load pattern (producer: bump `pending`, then
//! read `parked`; consumer: set `parked`, then read `pending`), which
//! needs `SeqCst` on those four accesses so at least one side sees the
//! other; `Release`/`Acquire` alone would let both miss. What remains is
//! a `notify_one` that lands after the consumer's re-check and before its
//! `wait_timeout`, since the producer takes no lock: that wake waits out
//! the timeout (20 ms on the processing thread, 50 ms on the writer), and
//! the rings hold the frames meanwhile.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// The counting wake-up; see the module doc.
#[derive(Debug)]
pub struct Wake {
    pending: AtomicU32,
    parked: AtomicBool,
    lock: Mutex<()>,
    condvar: Condvar,
}

impl Default for Wake {
    fn default() -> Self {
        Self::new()
    }
}

impl Wake {
    /// Nothing pending, nobody parked.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            parked: AtomicBool::new(false),
            lock: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    /// Producer side: one more pending wake; wakes a parked consumer.
    #[inline(always)]
    pub fn signal(&self) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.parked.load(Ordering::SeqCst) {
            self.condvar.notify_one();
        }
    }

    /// Consumer side: takes one pending wake, or waits for one up to
    /// `timeout`. Returns `true` when a wake was taken.
    pub fn wait(&self, timeout: Duration) -> bool {
        if self.try_take() {
            return true;
        }
        let guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.parked.store(true, Ordering::SeqCst);
        // Re-check after announcing the park so a signal between the first
        // check and the store is not missed.
        if self.try_take() {
            self.parked.store(false, Ordering::SeqCst);
            return true;
        }
        let (guard, _) = self
            .condvar
            .wait_timeout(guard, timeout)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.parked.store(false, Ordering::SeqCst);
        drop(guard);
        self.try_take()
    }

    /// Consumer side: a pending wake without waiting.
    pub fn try_take(&self) -> bool {
        let mut current = self.pending.load(Ordering::SeqCst);
        while current > 0 {
            match self.pending.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(seen) => current = seen,
            }
        }
        false
    }
}
