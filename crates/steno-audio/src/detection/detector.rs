//! Tells the app when another process opens the microphone (a call
//! starting) and when it lets go.
//! Swift: `Sources/StenoAudio/Detection/MeetingDetector.swift`.
//!
//! Reads [`ProcessAudioActivitySource::snapshot`] on every HAL change
//! notification and on a 1 s poll (the listener behaviour is undocumented,
//! so the poll is the safety net; on macOS a snapshot is a handful of
//! property reads), ignores its own PID, and debounces both edges by 2 s so a
//! flapping input yields one `MicrophoneOpened` and one
//! `MicrophoneReleased`. Worst case without a listener event: 3 s from the
//! microphone opening to the event. Every timer runs on the injected
//! [`Clock`], so tests drive a `ManualClock` and never sleep.
//!
//! Known v1 limit: when the reported holder releases the microphone and
//! another process already holds it in the same snapshot, the microphone
//! stays "open" and `holder` keeps naming the first process until it is
//! released again.

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use super::activity::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};
use crate::clock::{Cancel, Clock};

/// What the detector tells its subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetingEvent {
    /// Another process has held the microphone for the debounce.
    MicrophoneOpened {
        /// The holder's bundle identifier, when the HAL knows it.
        bundle_id: Option<String>,
        /// The holder's process id.
        pid: i32,
    },
    /// The holder let go for the debounce.
    MicrophoneReleased,
}

/// Polls and listens to a [`ProcessAudioActivitySource`] and debounces it
/// into [`MeetingEvent`]s; see the module doc.
pub struct MeetingDetector {
    core: Arc<Core>,
}

struct Core {
    source: Arc<dyn ProcessAudioActivitySource>,
    clock: Arc<dyn Clock>,
    ignoring_pids: BTreeSet<i32>,
    debounce: Duration,
    poll_interval: Duration,
    /// Held across a whole `evaluate`, snapshot included, so the listener
    /// and the poller apply their snapshots in the order they read them:
    /// a poll snapshot read before a change and applied after it would
    /// otherwise cancel the debounce the change armed. The Swift detector
    /// is an actor, which serialises the same way. `stop` holds it too, so
    /// no evaluation runs across it. Taken before `inner`.
    evaluating: Mutex<()>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    subscribers: Vec<Sender<MeetingEvent>>,
    threads: Vec<JoinHandle<()>>,
    /// Raised when the detector stops; the poll and listen threads watch it.
    running: Option<Cancel>,
    /// The debounce in flight, if armed.
    pending: Option<Cancel>,
    pending_generation: usize,
    /// The process reported as holding the microphone, `None` while released.
    holder: Option<ProcessAudioActivity>,
    /// What the last snapshot said, before debouncing.
    observed_active: Option<ProcessAudioActivity>,
}

impl MeetingDetector {
    /// How long a state must hold before it is reported.
    pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(2);
    /// Between snapshots when no notification arrives.
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

    /// `ignoring_pids` defaults to this process.
    #[must_use]
    pub fn new(
        source: Arc<dyn ProcessAudioActivitySource>,
        clock: Arc<dyn Clock>,
        ignoring_pids: Option<BTreeSet<i32>>,
        debounce: Duration,
        poll_interval: Duration,
    ) -> Self {
        // A pid fits i32 on every platform Steno runs on.
        #[allow(clippy::cast_possible_wrap)]
        let own = std::process::id() as i32;
        Self {
            core: Arc::new(Core {
                source,
                clock,
                ignoring_pids: ignoring_pids.unwrap_or_else(|| BTreeSet::from([own])),
                debounce,
                poll_interval,
                evaluating: Mutex::new(()),
                inner: Mutex::new(Inner::default()),
            }),
        }
    }

    /// The debounce in use.
    #[must_use]
    pub fn debounce(&self) -> Duration {
        self.core.debounce
    }

    /// The poll interval in use.
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        self.core.poll_interval
    }

    /// `start` has run and `stop` has not.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.core.lock().running.is_some()
    }

    /// The process reported as holding the microphone, `None` while released.
    #[must_use]
    pub fn holder(&self) -> Option<ProcessAudioActivity> {
        self.core.lock().holder.clone()
    }

    /// Events from now on. The channel closes when the detector stops.
    #[must_use]
    pub fn events(&self) -> Receiver<MeetingEvent> {
        let (sender, receiver) = channel();
        self.core.lock().subscribers.push(sender);
        receiver
    }

    /// Reads a first snapshot (an already-open microphone is reported after
    /// the debounce like any other), then listens and polls.
    pub fn start(&self) -> Result<(), ActivityError> {
        if self.is_running() {
            return Ok(());
        }
        self.core.evaluate(None)?;
        let running = Cancel::new();
        let changes = self.core.source.changes();
        let listener = {
            let core = Arc::downgrade(&self.core);
            let running = running.clone();
            std::thread::Builder::new()
                .name("steno-det-lis".into())
                .spawn(move || {
                    while !running.is_cancelled() {
                        match changes.recv_timeout(Duration::from_millis(100)) {
                            Ok(()) => {
                                if let Some(core) = core.upgrade() {
                                    core.evaluate_ignoring_errors(&running);
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .expect("spawn detector listener")
        };
        let poller = {
            let core = Arc::downgrade(&self.core);
            let clock = Arc::clone(&self.core.clock);
            let interval = self.core.poll_interval;
            let running = running.clone();
            std::thread::Builder::new()
                .name("steno-det-poll".into())
                .spawn(move || {
                    while clock.sleep(interval, &running) {
                        let Some(core) = core.upgrade() else { return };
                        core.evaluate_ignoring_errors(&running);
                    }
                })
                .expect("spawn detector poll")
        };
        let mut inner = self.core.lock();
        inner.running = Some(running);
        inner.threads.push(listener);
        inner.threads.push(poller);
        Ok(())
    }

    /// Stops the poll and listen threads, drops a pending debounce and forgets
    /// the holder; a second call does nothing.
    pub fn stop(&self) {
        // An evaluation in flight finishes first; one that waits for the
        // lock finds `running` cancelled and applies nothing.
        let evaluating = self
            .core
            .evaluating
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (running, pending, threads) = {
            let mut inner = self.core.lock();
            inner.pending_generation += 1;
            inner.holder = None;
            inner.observed_active = None;
            inner.subscribers.clear();
            (
                inner.running.take(),
                inner.pending.take(),
                std::mem::take(&mut inner.threads),
            )
        };
        if let Some(running) = running {
            running.cancel();
        }
        drop(evaluating);
        if let Some(pending) = pending {
            pending.cancel();
        }
        for thread in threads {
            let _ = thread.join();
        }
    }
}

impl Drop for MeetingDetector {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Core {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn evaluate_ignoring_errors(self: &Arc<Self>, running: &Cancel) {
        let _ = self.evaluate(Some(running));
    }

    /// Compares the snapshot with what was reported and arms or disarms
    /// the debounce timer. `running` is the listener's or the poller's
    /// flag (`None` for `start`'s first read): once `stop` raised it,
    /// nothing is applied.
    fn evaluate(self: &Arc<Self>, running: Option<&Cancel>) -> Result<(), ActivityError> {
        let _evaluating = self
            .evaluating
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if running.is_some_and(Cancel::is_cancelled) {
            return Ok(());
        }
        let mut active: Vec<ProcessAudioActivity> = self
            .source
            .snapshot()?
            .into_iter()
            .filter(|a| a.is_running_input && !self.ignoring_pids.contains(&a.pid))
            .collect();
        active.sort_by_key(|a| a.pid);
        let mut inner = self.lock();
        // Keep reporting the same holder while it stays active.
        let holder_pid = inner.holder.as_ref().map(|h| h.pid);
        let observed_pid = inner.observed_active.as_ref().map(|o| o.pid);
        let current = active
            .iter()
            .find(|a| Some(a.pid) == holder_pid)
            .or_else(|| active.iter().find(|a| Some(a.pid) == observed_pid))
            .or_else(|| active.first())
            .cloned();
        inner.observed_active.clone_from(&current);

        let want_open = current.is_some();
        let is_open = inner.holder.is_some();
        if want_open == is_open {
            // Back to the reported state within the debounce: forget the edge.
            Self::cancel_pending(&mut inner);
            return Ok(());
        }
        if inner.pending.is_some() {
            return Ok(());
        }
        inner.pending_generation += 1;
        let generation = inner.pending_generation;
        let cancel = Cancel::new();
        inner.pending = Some(cancel.clone());
        let core: Weak<Core> = Arc::downgrade(self);
        let clock = Arc::clone(&self.clock);
        let debounce = self.debounce;
        let thread = std::thread::Builder::new()
            .name("steno-debounce".into())
            .spawn(move || {
                if clock.sleep(debounce, &cancel)
                    && let Some(core) = core.upgrade()
                {
                    core.debounce_elapsed(generation);
                }
            })
            .expect("spawn detector debounce");
        inner.threads.push(thread);
        // Finished threads pile up only while the detector runs; keep the
        // list short.
        inner.threads.retain(|t| !t.is_finished());
        Ok(())
    }

    fn cancel_pending(inner: &mut Inner) {
        if let Some(pending) = inner.pending.take() {
            pending.cancel();
        }
        inner.pending_generation += 1;
    }

    fn debounce_elapsed(&self, generation: usize) {
        let (event, subscribers) = {
            let mut inner = self.lock();
            if generation != inner.pending_generation {
                return;
            }
            inner.pending = None;
            let event = match (&inner.observed_active, &inner.holder) {
                (Some(process), None) => {
                    let event = MeetingEvent::MicrophoneOpened {
                        bundle_id: process.bundle_id.clone(),
                        pid: process.pid,
                    };
                    inner.holder = Some(process.clone());
                    Some(event)
                }
                (None, Some(_)) => {
                    inner.holder = None;
                    Some(MeetingEvent::MicrophoneReleased)
                }
                _ => None,
            };
            (event, inner.subscribers.clone())
        };
        if let Some(event) = event {
            for subscriber in subscribers {
                let _ = subscriber.send(event.clone());
            }
        }
    }
}
