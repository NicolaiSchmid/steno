//! A clock the test advances by hand.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use steno_core::async_trait;
use tokio::sync::{Notify, oneshot};

use crate::Clock;

/// A [`Clock`] that only moves when a test calls [`advance`](Self::advance).
/// Sleepers register their deadline and wake when the clock passes it; a
/// dropped sleep (a cancelled attempt) unregisters itself, so
/// [`pending_sleepers`](Self::pending_sleepers) counts exactly the waits in
/// progress.
#[derive(Default)]
pub struct ManualClock {
    state: Mutex<State>,
    sleepers_changed: Notify,
}

#[derive(Default)]
struct State {
    now: Duration,
    next_id: u64,
    sleepers: Vec<Sleeper>,
}

struct Sleeper {
    id: u64,
    deadline: Duration,
    wake: oneshot::Sender<()>,
}

impl ManualClock {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(ManualClock::default())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves the clock forward and wakes every sleeper whose deadline has
    /// passed.
    pub fn advance(&self, by: Duration) {
        let due: Vec<Sleeper> = {
            let mut state = self.state();
            state.now += by;
            let now = state.now;
            let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.sleepers)
                .into_iter()
                .partition(|sleeper| sleeper.deadline <= now);
            state.sleepers = waiting;
            due
        };
        for sleeper in due {
            let _ = sleeper.wake.send(());
        }
        self.sleepers_changed.notify_waiters();
    }

    /// The offset since the clock was created.
    #[must_use]
    pub fn offset(&self) -> Duration {
        self.state().now
    }

    /// Sleeps registered and not yet woken.
    #[must_use]
    pub fn pending_sleepers(&self) -> usize {
        self.state().sleepers.len()
    }

    /// Waits until at least `count` sleepers are registered; `false` when
    /// that has not happened within `timeout` of wall time.
    pub async fn wait_for_sleepers(&self, count: usize, timeout: Duration) -> bool {
        let wait = super::wait_until(&self.sleepers_changed, || self.pending_sleepers() >= count);
        tokio::time::timeout(timeout, wait).await.is_ok()
    }

    fn unregister(&self, id: u64) {
        self.state().sleepers.retain(|sleeper| sleeper.id != id);
        self.sleepers_changed.notify_waiters();
    }
}

/// Removes the sleeper when the sleep future is dropped before it woke.
struct Registration<'a> {
    clock: &'a ManualClock,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.clock.unregister(self.id);
    }
}

#[async_trait]
impl Clock for ManualClock {
    async fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let (wake, woken) = oneshot::channel();
        let id = {
            let mut state = self.state();
            let id = state.next_id;
            state.next_id += 1;
            let deadline = state.now + duration;
            state.sleepers.push(Sleeper { id, deadline, wake });
            id
        };
        self.sleepers_changed.notify_waiters();
        let registration = Registration { clock: self, id };
        let _ = woken.await;
        drop(registration);
    }

    fn now(&self) -> Duration {
        self.offset()
    }
}
