//! The gate every in-app playback goes through: no playback while
//! recording. On macOS the call capture's process tap includes Steno's own
//! process, so anything Steno played would land in the system lane. The
//! capture session holds the gate while it records; taking the hold stops
//! every playback that runs.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

/// What a player runs when a recording starts while it plays: stop or
/// pause, then return. It runs on the thread that starts the recording,
/// with the gate unlocked, and must not block.
type OnStop = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Gate {
    recordings: usize,
    next_permit: u64,
    /// The permits playing now, with what stops each.
    playing: BTreeMap<u64, OnStop>,
}

/// The playback gate. A cheap handle: clones share one gate.
#[derive(Clone)]
pub struct Playback {
    gate: Arc<Mutex<Gate>>,
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gate = self.lock();
        f.debug_struct("Playback")
            .field("recordings", &gate.recordings)
            .field("playing", &gate.playing.len())
            .finish()
    }
}

/// The answer to a playback asked for while a recording runs, in words the
/// UI can show as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Playback is off while Steno records. It works again once the recording stops.")]
pub struct PlaybackRefused;

impl Playback {
    /// A private gate, for a test that must not share [`Self::global`].
    #[doc(hidden)]
    #[must_use]
    #[allow(
        clippy::new_without_default,
        reason = "a second gate is for tests only"
    )]
    pub fn new() -> Self {
        Self {
            gate: Arc::default(),
        }
    }

    /// The process's gate, held by the capture session and asked by every
    /// player.
    #[must_use]
    pub fn global() -> Self {
        static GLOBAL: LazyLock<Playback> = LazyLock::new(Playback::new);
        GLOBAL.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a recording holds the gate now.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.lock().recordings > 0
    }

    /// Permission to play, or [`PlaybackRefused`] while a recording runs.
    /// `on_stop` runs once if a recording starts while the permit is held
    /// (stop or pause there, and do not block); dropping the permit ends
    /// the playback as far as the gate is concerned.
    ///
    /// A player asks the process's gate before every playback:
    ///
    /// ```
    /// use steno_audio::Playback;
    ///
    /// match Playback::global().begin(|| { /* pause the player */ }) {
    ///     // Start under the gate's lock, then play while the permit is
    ///     // held; drop it when the clip ends.
    ///     Ok(permit) => assert!(permit.start(|| { /* start the player */ }).is_ok()),
    ///     // A recording runs: show the refusal as it is worded.
    ///     Err(refused) => assert!(refused.to_string().starts_with("Playback is off")),
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// [`PlaybackRefused`] while a recording holds the gate.
    pub fn begin(
        &self,
        on_stop: impl FnOnce() + Send + 'static,
    ) -> Result<PlaybackPermit, PlaybackRefused> {
        let mut gate = self.lock();
        if gate.recordings > 0 {
            return Err(PlaybackRefused);
        }
        let id = gate.next_permit;
        gate.next_permit += 1;
        gate.playing.insert(id, Box::new(on_stop));
        Ok(PlaybackPermit {
            playback: self.clone(),
            id,
        })
    }

    /// Holds the gate for one recording until the hold is dropped, and
    /// stops every playback (each `on_stop` runs on this thread, unlocked,
    /// before this returns). Holds count.
    #[must_use = "the gate opens again when the hold is dropped"]
    pub(crate) fn hold_for_recording(&self) -> RecordingHold {
        let stopping = {
            let mut gate = self.lock();
            gate.recordings += 1;
            std::mem::take(&mut gate.playing)
        };
        // Built before the players run, so a panic in one still releases.
        let hold = RecordingHold {
            playback: self.clone(),
        };
        for on_stop in stopping.into_values() {
            on_stop();
        }
        hold
    }
}

/// Permission to play, from [`Playback::begin`].
#[derive(Debug)]
pub struct PlaybackPermit {
    playback: Playback,
    id: u64,
}

impl PlaybackPermit {
    /// Runs `start` (the player's start) under the gate's lock, unless a
    /// recording has started since the permit was given, so nothing starts
    /// once a hold is taken. `start` must not play the clip through nor ask
    /// the gate again.
    ///
    /// # Errors
    ///
    /// [`PlaybackRefused`] when a recording has started since the permit
    /// was given; `start` has not run.
    pub fn start<R>(&self, start: impl FnOnce() -> R) -> Result<R, PlaybackRefused> {
        let gate = self.playback.lock();
        if !gate.playing.contains_key(&self.id) {
            return Err(PlaybackRefused);
        }
        Ok(start())
    }

    /// Whether a recording has started since the permit was given; its
    /// `on_stop` has run, and the player must not play on.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        !self.playback.lock().playing.contains_key(&self.id)
    }
}

impl Drop for PlaybackPermit {
    fn drop(&mut self) {
        // Taken out under the lock and dropped after it: the closure's
        // captures may own anything.
        let on_stop = self.playback.lock().playing.remove(&self.id);
        drop(on_stop);
    }
}

/// One recording's hold on the gate, from [`Playback::hold_for_recording`];
/// released on drop.
#[derive(Debug)]
pub(crate) struct RecordingHold {
    playback: Playback,
}

impl Drop for RecordingHold {
    fn drop(&mut self) {
        let mut gate = self.playback.lock();
        gate.recordings = gate.recordings.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn playback_is_refused_while_a_recording_holds_the_gate() {
        let playback = Playback::new();
        let hold = playback.hold_for_recording();
        assert!(playback.is_recording());
        assert_eq!(playback.begin(|| {}).unwrap_err(), PlaybackRefused);
        assert_eq!(
            PlaybackRefused.to_string(),
            "Playback is off while Steno records. It works again once the recording stops."
        );
        drop(hold);
        assert!(!playback.is_recording());
        let permit = playback.begin(|| {}).expect("allowed once released");
        assert!(!permit.is_stopped());
    }

    #[test]
    fn playback_that_runs_is_stopped_once_when_a_recording_starts() {
        let playback = Playback::new();
        let stops = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&stops);
        let permit = playback
            .begin(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let ended = playback.begin(|| panic!("a dropped permit is not stopped"));
        drop(ended);
        let hold = playback.hold_for_recording();
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "on_stop ran before the hold returned"
        );
        assert!(permit.is_stopped());
        drop(hold);
        let _again = playback.hold_for_recording();
        assert_eq!(stops.load(Ordering::SeqCst), 1, "once, not per recording");
    }

    /// The gate opens only when the last of two recordings releases it.
    #[test]
    fn two_recordings_keep_the_gate_shut_until_both_release_it() {
        let playback = Playback::new();
        let first = playback.hold_for_recording();
        let second = playback.clone().hold_for_recording();
        drop(first);
        assert_eq!(playback.begin(|| {}).unwrap_err(), PlaybackRefused);
        drop(second);
        assert!(playback.begin(|| {}).is_ok());
    }

    /// A panic while the gate is held, in the recording or in a player's
    /// `on_stop`, still releases it.
    #[test]
    fn a_panic_releases_the_gate() {
        let playback = Playback::new();
        let held = playback.clone();
        let panicked = std::thread::spawn(move || {
            let _hold = held.hold_for_recording();
            panic!("the capture failed");
        })
        .join();
        assert!(panicked.is_err());
        assert!(!playback.is_recording());

        let _permit = playback.begin(|| panic!("a player that panics")).unwrap();
        let held = playback.clone();
        assert!(
            std::thread::spawn(move || held.hold_for_recording())
                .join()
                .is_err()
        );
        assert!(!playback.is_recording());
    }
}
