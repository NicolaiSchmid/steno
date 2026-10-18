//! The consumer wake-up behind the rings and the level slot: a counting
//! signal the producer raises without a lock (an atomic increment, plus one
//! `notify_one` when the consumer is parked) and the consumer waits on with
//! a timeout. The hand-off needs `SeqCst` on `pending` and `parked` so at
//! least one side sees the other; a wake that still slips past costs one
//! timeout, while the rings hold the frames.

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
