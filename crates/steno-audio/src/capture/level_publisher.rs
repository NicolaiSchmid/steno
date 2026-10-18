//! The level publisher: a thread of its own that hands the processing
//! thread's levels on whenever their generation in [`LevelSlot`] changed,
//! so a slow file sync on the writer thread never freezes the meter. It
//! waits on the slot's wake, and polls every [`LevelPublisher::POLL`] in
//! case a wake was lost.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use super::LaneLevels;
use crate::realtime::LevelSlot;

/// Runs on the publisher's thread with each change of the levels.
pub type LevelsHandler = Box<dyn Fn(LaneLevels) + Send>;

/// The publisher's handle; `stop` hands on a last change and joins it.
pub struct LevelPublisher {
    slot: Arc<LevelSlot>,
    stop_requested: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LevelPublisher {
    /// The longest the thread sleeps without a wake, in case one was
    /// lost: the default level interval, 100 ms.
    pub const POLL: Duration = Duration::from_millis(100);

    /// Spawns the thread over `slot`; it calls `on_levels` each time the
    /// levels' generation changes from the one at this call.
    #[must_use]
    pub fn start(slot: Arc<LevelSlot>, on_levels: LevelsHandler) -> Self {
        let stop_requested = Arc::new(AtomicBool::new(false));
        // Read here, not on the thread: a publish before it first runs is
        // a change too.
        let mut last_generation = slot.current_generation();
        let thread = std::thread::Builder::new()
            .name("steno-levels".into())
            .spawn({
                let slot = Arc::clone(&slot);
                let stop = Arc::clone(&stop_requested);
                move || {
                    let mut publish_if_changed = || {
                        let generation = slot.current_generation();
                        if generation != last_generation {
                            last_generation = generation;
                            on_levels(slot.levels());
                        }
                    };
                    while !stop.load(Ordering::Acquire) {
                        if slot.wake().wait(Self::POLL) {
                            // Wakes from publishes nobody waited for are
                            // spent; the generation says what changed.
                            while slot.wake().try_take() {}
                        }
                        publish_if_changed();
                    }
                    publish_if_changed();
                }
            })
            .expect("spawn level publisher thread");
        Self {
            slot,
            stop_requested,
            thread: Some(thread),
        }
    }

    /// Hands on the last change and joins the thread; a second call does
    /// nothing.
    pub fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.stop_requested.store(true, Ordering::Release);
            self.slot.wake().signal();
            let _ = thread.join();
        }
    }
}

impl Drop for LevelPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;
    use crate::capture::LaneLevel;

    fn level(rms: f32) -> LaneLevel {
        LaneLevel { rms, peak: rms }
    }

    /// Each publish into the slot reaches the handler, once, and nothing
    /// else does.
    #[test]
    fn every_publish_reaches_the_handler_once() {
        let slot = Arc::new(LevelSlot::new(false));
        slot.publish(level(-60.0), None);
        let (sender, levels) = channel();
        let mut publisher = LevelPublisher::start(
            Arc::clone(&slot),
            Box::new(move |levels| sender.send(levels).unwrap()),
        );
        for rms in [-30.0, -20.0, -10.0] {
            slot.publish(level(rms), None);
            let received = levels.recv_timeout(crate::testing::HANG_GUARD).unwrap();
            assert_eq!(received.mic, level(rms));
        }
        publisher.stop();
        assert!(levels.try_recv().is_err(), "a level went out twice");
    }

    /// A change not yet handed on when `stop` came goes out before the
    /// thread ends.
    #[test]
    fn the_stop_hands_on_the_last_change() {
        let slot = Arc::new(LevelSlot::new(true));
        let (sender, levels) = channel();
        let mut publisher = LevelPublisher::start(
            Arc::clone(&slot),
            Box::new(move |levels| sender.send(levels).unwrap()),
        );
        slot.publish(LaneLevel::SILENCE, Some(level(-6.0)));
        publisher.stop();
        let last = levels.try_iter().last().expect("the last change went out");
        assert_eq!(last.system, Some(level(-6.0)));
        publisher.stop();
    }
}
