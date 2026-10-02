//! A [`Clock`] the test advances by hand. Swift: `ManualClock` in
//! `Sources/StenoCore/Testing`.
//!
//! `sleep` registers a sleeper at `now + duration` and parks until the
//! clock has been advanced past it or the sleep is cancelled.
//! `wait_for_sleepers(n)` lets a test synchronise with the code under test
//! (the rebuild's backoff, the detector's poll and debounce) without wall
//! time entering the assertions.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::clock::{Cancel, Clock};

#[derive(Debug, Default)]
pub struct ManualClock {
    state: Mutex<State>,
    condvar: Condvar,
}

#[derive(Debug, Default)]
struct State {
    now: Duration,
    /// The sleeps in progress: a ticket and the deadline. `advance` removes
    /// every sleeper whose deadline has passed at once, so a test that
    /// advanced the clock sees `pending_sleepers()` drop before the
    /// sleeping thread has even woken, as Swift's `ManualClock` did.
    sleepers: Vec<(u64, Duration)>,
    next_ticket: u64,
}

impl ManualClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward and wakes every sleeper whose deadline has
    /// passed (a sleeper wakes at its deadline however far the clock moves
    /// past it).
    pub fn advance(&self, by: Duration) {
        let mut state = self.lock();
        state.now += by;
        let now = state.now;
        state.sleepers.retain(|(_, deadline)| *deadline > now);
        drop(state);
        self.condvar.notify_all();
    }

    /// Sleeps in progress right now.
    #[must_use]
    pub fn pending_sleepers(&self) -> usize {
        self.lock().sleepers.len()
    }

    /// Polls until exactly `count` sleeps are in progress, for up to about
    /// two seconds of wall time; `true` when they are.
    pub fn wait_for_sleepers(&self, count: usize) -> bool {
        for _ in 0..2_000 {
            if self.pending_sleepers() == count {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.pending_sleepers() == count
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.lock().now
    }

    fn sleep(&self, duration: Duration, cancel: &Cancel) -> bool {
        let mut state = self.lock();
        let deadline = state.now + duration;
        if duration.is_zero() {
            return !cancel.is_cancelled();
        }
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.sleepers.push((ticket, deadline));
        let outcome = loop {
            if cancel.is_cancelled() {
                break false;
            }
            if state.now >= deadline {
                break true;
            }
            // A cancel raises its own condvar, not this one, so the wait is
            // bounded and the flag re-read; cancellation latency is 2 ms.
            state = self
                .condvar
                .wait_timeout(state, Duration::from_millis(2))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        };
        // Already gone when `advance` woke us; still here after a cancel.
        state.sleepers.retain(|(t, _)| *t != ticket);
        outcome
    }
}
