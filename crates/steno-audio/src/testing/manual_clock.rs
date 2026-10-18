//! A [`Clock`] the test advances by hand. `sleep` parks until the clock
//! passes its deadline or the sleep is cancelled; `wait_for_sleepers(n)`
//! lets a test synchronise with the code under test without wall time.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::HANG_GUARD;
use crate::clock::{Cancel, Clock};

/// A clock tests move by hand.
#[derive(Debug, Default)]
pub struct ManualClock {
    state: Mutex<State>,
    condvar: Condvar,
}

#[derive(Debug, Default)]
struct State {
    now: Duration,
    /// The sleeps in progress: a ticket and the deadline. `advance` removes
    /// passed sleepers at once, so `pending_sleepers()` drops before the
    /// sleeping thread has woken.
    sleepers: Vec<(u64, Duration)>,
    next_ticket: u64,
}

impl ManualClock {
    /// At zero, nobody sleeping.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward and wakes every sleeper whose deadline has
    /// passed.
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

    /// Polls until exactly `count` sleeps are in progress and returns
    /// `true`, for an `assert!` that names the sleeper. Waits up to
    /// [`HANG_GUARD`] of wall time, since a loaded CI host is slow.
    ///
    /// # Panics
    ///
    /// When the count is still not `count` after [`HANG_GUARD`]: the code
    /// under test hung, and the message says how many sleepers it found.
    #[track_caller]
    pub fn wait_for_sleepers(&self, count: usize) -> bool {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            let found = self.pending_sleepers();
            if found == count {
                return true;
            }
            assert!(
                Instant::now() < deadline,
                "hung: {count} sleepers expected on the clock, found {found} after {HANG_GUARD:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
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
