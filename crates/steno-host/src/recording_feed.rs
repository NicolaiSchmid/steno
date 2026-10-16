//! The `recording` topic's feed: one thread (`steno-host-recording`) that
//! reads the recorder's status and sends the snapshot, with no host lock
//! taken, so the levels, `Stopping` and a stop's hook never wait for a
//! reload, a command or a publish under the view-model lock (decision 3 of
//! `.plans/2026-10-12-store-off-the-workers.md`).
//!
//! - **A poke** sets "poked", unparks the thread and returns: the
//!   recorder's hook ([`Host::recorder_changed`](crate::Host::recorder_changed)),
//!   on the levels thread and on the stop's thread alike, and the start
//!   command.
//! - **Each pass** reads "page ready" first, then swaps "poked" to false,
//!   and "resend" too once the page is ready, then reads `status()` (the
//!   recorder's status lock, released before anything else), and sends
//!   nothing unless the page was ready when the pass began. A snapshot goes out
//!   at once when "resend" was set or when anything but its levels differs
//!   from the last one sent; after a poke, one that moved only the levels,
//!   or did not move, waits out the rest of [`RECORDING_INTERVAL`] and goes
//!   out when it ends, so every hook still gets its snapshot. Only a
//!   snapshot that went out stamps the interval and becomes the last one
//!   sent. A wake with neither flag set and no snapshot held back (the
//!   unpark of a poke an earlier pass already answered) reads and sends
//!   nothing. The sink is taken from its slot, then emitted to with the
//!   slot released.
//! - **A move** of the recorder's state or meeting, before `page.ready`
//!   too, asks the reload owner for a pass
//!   ([`Host::request_reload`](crate::Host::request_reload)): a start
//!   wrote the meeting `recording`, a stop queued it, and the list must
//!   follow, since the web app tells a live row from one left `recording`
//!   by the recorder's state.
//! - **`page.ready`** sets "resend", then "page ready", then unparks the
//!   thread, and never touches the last snapshot sent: every `page.ready`,
//!   a reloaded page's or another window's, gets the current `recording`
//!   once, from an idle recorder too. The order makes "once" hold: a pass
//!   that finds the page ready finds "resend" set, and one that began
//!   before the page was ready leaves "resend" for the next.
//!
//! Only the feed's thread touches the last snapshot sent and when it went,
//! so they need no lock; other threads reach it through the three flags
//! and the unpark. The thread holds a `Weak` on the host and ends once no
//! host is left. Each topic is a full state, so the order between `recording`
//! and a publish's batch decides nothing. Swift: the main window's
//! `TopicPublisher` throttling `.recording` (`MainWindowBridge`), on the
//! main actor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use steno_bridge::{BridgeEvent, BridgeTopic, EventSink, RecordingSnapshot, RecordingState};
use uuid::Uuid;

use crate::main_window::snapshots::recording_snapshot;
use crate::services::RecorderStatus;

/// 20 Hz: the level meter's rate on the page. Swift:
/// `MainWindowBridge.recordingInterval`.
pub const RECORDING_INTERVAL: Duration = Duration::from_millis(50);

/// The recorder's state and meeting, whose move asks for a reload.
pub(crate) type Position = (RecordingState, Option<Uuid>);

/// What the feed reads and where it sends: the host's shared state. Each
/// method takes at most one leaf lock, briefly, and none of the host's.
pub(crate) trait Source: Send + Sync {
    fn feed(&self) -> &RecordingFeed;
    /// The recorder's status.
    fn status(&self) -> RecorderStatus;
    /// The sink attached now, the slot's lock released.
    fn sink(&self) -> Option<Arc<dyn EventSink>>;
    /// The recorder's state or meeting moved: asks the reload owner for a
    /// pass, which only sets a flag and wakes it.
    fn moved(&self);
}

/// The flags other threads set, and the handle that wakes the feed's
/// thread.
pub(crate) struct RecordingFeed {
    page_ready: AtomicBool,
    /// The next pass sends the current snapshot, changed or not.
    resend: AtomicBool,
    /// A poke came since the last pass: the status may have moved. Set at
    /// first, so the first pass reads the status even when a move came
    /// before the thread could be woken.
    poked: AtomicBool,
    thread: OnceLock<thread::Thread>,
    /// The thread's handle, so a test can tell that it ended.
    #[cfg(test)]
    joined: std::sync::Mutex<Option<thread::JoinHandle<()>>>,
    /// Runs once, in the first pass, between its swaps and its send check,
    /// so a test can land a `page.ready` there.
    #[cfg(test)]
    pause: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl RecordingFeed {
    pub(crate) fn new() -> Self {
        RecordingFeed {
            page_ready: AtomicBool::new(false),
            resend: AtomicBool::new(false),
            poked: AtomicBool::new(true),
            thread: OnceLock::new(),
            #[cfg(test)]
            joined: std::sync::Mutex::default(),
            #[cfg(test)]
            pause: std::sync::Mutex::default(),
        }
    }

    /// Starts the feed's thread over `source`, whose recorder stood at
    /// `seen` before the host loaded its view models, so a move after that
    /// asks for a reload.
    pub(crate) fn start<S: Source + 'static>(
        source: &Arc<S>,
        seen: Position,
    ) -> std::io::Result<()> {
        let weak = Arc::downgrade(source);
        let handle = thread::Builder::new()
            .name("steno-host-recording".to_owned())
            .spawn(move || run(&weak, seen))?;
        let feed = source.feed();
        let _ = feed.thread.set(handle.thread().clone());
        // A poke between the first pass and the `set` above woke nothing.
        handle.thread().unpark();
        #[cfg(test)]
        {
            *feed.joined.lock().unwrap() = Some(handle);
        }
        Ok(())
    }

    /// The recorder's status may have moved: wakes the feed's thread for
    /// a pass that sends it; takes no lock.
    pub(crate) fn poke(&self) {
        self.poked.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// `page.ready`: the next pass sends the current snapshot at once,
    /// changed or not.
    pub(crate) fn page_did_become_ready(&self) {
        self.resend.store(true, Ordering::SeqCst);
        self.page_ready.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// Wakes the feed's thread; at the host's drop, so it ends.
    pub(crate) fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    /// The thread's handle, taken.
    #[cfg(test)]
    pub(crate) fn take_thread(&self) -> Option<thread::JoinHandle<()>> {
        self.joined.lock().unwrap().take()
    }
}

/// The feed's thread: a pass per wake, asleep in between, until no host
/// is left.
fn run<S: Source>(source: &Weak<S>, mut seen: Position) {
    let mut sent = Sent::default();
    let mut held = false;
    loop {
        let Some(strong) = source.upgrade() else {
            return;
        };
        let wait = pass(&*strong, &mut seen, &mut sent, held);
        held = wait.is_some();
        drop(strong);
        match wait {
            Some(wait) => thread::park_timeout(wait),
            None => thread::park(),
        }
    }
}

/// One pass (the module doc), `held` when the last one held a snapshot
/// back; how long until the snapshot this one holds back may go out,
/// `None` when it holds none.
fn pass<S: Source>(
    source: &S,
    seen: &mut Position,
    sent: &mut Sent,
    held: bool,
) -> Option<Duration> {
    let feed = source.feed();
    let ready = feed.page_ready.load(Ordering::SeqCst);
    let resend = ready && feed.resend.swap(false, Ordering::SeqCst);
    let poked = feed.poked.swap(false, Ordering::SeqCst);
    #[cfg(test)]
    if let Some(pause) = feed.pause.lock().unwrap().take() {
        pause();
    }
    if !(resend || poked || held) {
        return None;
    }
    let status = source.status();
    let position = (status.state, status.meeting_id);
    if *seen != position {
        *seen = position;
        source.moved();
    }
    if !ready {
        return None;
    }
    let snapshot = recording_snapshot(&status);
    let now = Instant::now();
    if let Some(wait) = sent.wait(&snapshot, resend, now) {
        return Some(wait);
    }
    let sink = source.sink()?;
    let payload = serde_json::to_value(&snapshot).ok()?;
    sink.emit(BridgeEvent::new(BridgeTopic::Recording, payload));
    sent.went_out(snapshot, now);
    None
}

/// The last snapshot that went out and when: the throttle's state.
#[derive(Debug, Default)]
struct Sent {
    last: Option<(RecordingSnapshot, Instant)>,
}

impl Sent {
    /// How long `snapshot` must wait at `now`, `None` when it goes out
    /// now: at once on `resend`, for the first one, or when anything but
    /// the levels moved; otherwise once the interval since the last one
    /// that went out has passed.
    fn wait(&self, snapshot: &RecordingSnapshot, resend: bool, now: Instant) -> Option<Duration> {
        let (last, at) = self.last.as_ref()?;
        if resend || without_level(last) != without_level(snapshot) {
            return None;
        }
        let left = RECORDING_INTERVAL.saturating_sub(now.saturating_duration_since(*at));
        (!left.is_zero()).then_some(left)
    }

    /// `snapshot` went out at `now`.
    fn went_out(&mut self, snapshot: RecordingSnapshot, now: Instant) {
        self.last = Some((snapshot, now));
    }
}

fn without_level(snapshot: &RecordingSnapshot) -> RecordingSnapshot {
    RecordingSnapshot {
        level: None,
        ..snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use steno_bridge::{
        BridgeHost as _, CaptureMode, ListFilter, SetFilterParams, StartRecordingParams,
    };
    use steno_core::Store;

    use super::*;
    use crate::fakes::FakeServices;
    use crate::host::TOPICS;
    use crate::services::LaneLevels;
    use crate::{Host, HostConfig};

    /// What "live" means here: one hop through the feed's thread, loose
    /// enough for a loaded machine at `nice 19`.
    const LIVE: Duration = Duration::from_millis(100);

    /// How long a test waits for what must come before it fails as hung.
    const PATIENCE: Duration = Duration::from_secs(5);

    #[derive(Default)]
    struct Sink(Mutex<Vec<(Instant, BridgeEvent)>>);

    impl EventSink for Sink {
        fn emit(&self, event: BridgeEvent) {
            self.0.lock().unwrap().push((Instant::now(), event));
        }
    }

    impl Sink {
        fn recordings(&self) -> Vec<(Instant, serde_json::Value)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, event)| event.topic == BridgeTopic::Recording)
                .map(|(at, event)| (*at, event.payload.clone()))
                .collect()
        }

        fn topics(&self) -> Vec<BridgeTopic> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|(_, event)| event.topic)
                .collect()
        }

        /// Waits for the `n`th `recording` snapshot, `what` ([`found`]);
        /// when it arrived and its payload.
        fn nth_recording(&self, n: usize, what: &str) -> (Instant, serde_json::Value) {
            found(what, || self.recordings().into_iter().nth(n - 1))
        }
    }

    fn host() -> (Host, Arc<FakeServices>, Arc<Sink>) {
        let now = steno_core::json::parse_date("2026-09-29T12:50:00.000Z").unwrap();
        let fakes = Arc::new(FakeServices::new(now));
        let store = Arc::new(Store::in_memory().unwrap());
        let host = Host::new(store, fakes.services(), HostConfig::default()).unwrap();
        let sink = Arc::new(Sink::default());
        host.attach(sink.clone());
        (host, fakes, sink)
    }

    fn recording_with(fakes: &FakeServices, mic: f64) {
        fakes.recorder.set_status(RecorderStatus {
            state: RecordingState::Recording,
            meeting_id: Some(Uuid::from_u128(1)),
            levels: Some(LaneLevels {
                mic,
                system: Some(0.0),
            }),
            ..RecorderStatus::idle()
        });
    }

    /// P40 check 21: before `page.ready` neither a level update nor a
    /// state change reaches the sink; the first snapshot goes out once it
    /// came, and carries the status as it is then.
    #[test]
    fn the_recording_feed_sends_nothing_before_page_ready() {
        let (host, fakes, sink) = host();
        recording_with(&fakes, 0.1);
        host.recorder_changed();
        recording_with(&fakes, 0.2);
        host.recorder_changed();
        fakes.recorder.set_status(RecorderStatus {
            state: RecordingState::Stopping,
            ..RecorderStatus::idle()
        });
        host.recorder_changed();
        thread::sleep(4 * RECORDING_INTERVAL);
        assert!(sink.recordings().is_empty(), "nothing before page.ready");

        host.page_ready().unwrap();
        let (_, first) = sink.nth_recording(1, "the first snapshot after page.ready");
        assert_eq!(first["state"], "stopping");
        thread::sleep(2 * RECORDING_INTERVAL);
        assert_eq!(sink.recordings().len(), 1);
    }

    /// P40 check 21: with the view-model lock held (as a reload or a save
    /// that waits on the store holds it), level updates reach the sink
    /// within [`LIVE`] of their hook, and so does `Stopping`; each hook
    /// returns at once.
    #[test]
    fn the_recording_feed_runs_while_the_view_models_are_held() {
        let (host, fakes, sink) = host();
        host.page_ready().unwrap();
        sink.nth_recording(1, "page.ready's snapshot");
        let held = host.hold_view_models();
        for step in 1..=5u8 {
            let mic = f64::from(step) / 10.0;
            recording_with(&fakes, mic);
            let poked = Instant::now();
            host.recorder_changed();
            assert!(
                poked.elapsed() < LIVE,
                "the hook took {:?}",
                poked.elapsed()
            );
            let reached = found("the level update", || {
                sink.recordings()
                    .into_iter()
                    .find(|(_, payload)| {
                        payload["level"]["mic"]
                            .as_f64()
                            .is_some_and(|level| (level - mic).abs() < 1e-9)
                    })
                    .map(|(at, _)| at)
            });
            let took = reached.saturating_duration_since(poked);
            // A level update may wait out the rest of the interval.
            assert!(
                took < RECORDING_INTERVAL + LIVE,
                "level {mic} reached the sink {took:?} after its hook"
            );
        }
        fakes.recorder.set_status(RecorderStatus {
            state: RecordingState::Stopping,
            ..RecorderStatus::idle()
        });
        let poked = Instant::now();
        host.recorder_changed();
        let reached = found("Stopping", || {
            sink.recordings()
                .into_iter()
                .find(|(_, payload)| payload["state"] == "stopping")
                .map(|(at, _)| at)
        });
        let took = reached.saturating_duration_since(poked);
        assert!(
            took < LIVE,
            "Stopping reached the sink {took:?} after its hook"
        );
        drop(held);
    }

    /// P40 check 21: a second `page.ready` (a reloaded page, another
    /// window) gets the current `recording` once more, though the recorder
    /// is idle and its status has not moved since the first.
    #[test]
    fn a_second_page_ready_gets_the_recording_again() {
        let (host, _fakes, sink) = host();
        host.page_ready().unwrap();
        sink.nth_recording(1, "the first page.ready's");
        thread::sleep(2 * RECORDING_INTERVAL);
        let before = sink.recordings().len();
        host.page_ready().unwrap();
        let (_, again) = sink.nth_recording(before + 1, "the second page.ready's");
        assert_eq!(again["state"], "idle");
        thread::sleep(2 * RECORDING_INTERVAL);
        assert_eq!(sink.recordings().len(), before + 1, "once per page.ready");
    }

    /// P40 check 21, the throttle on its own clock: a level update held
    /// back neither becomes the last snapshot sent nor restarts the
    /// interval, and goes out when the interval since the last one that
    /// went out ends; two level updates inside one interval fold into one
    /// send, the later one's. Anything but the levels, and a resend, goes
    /// at once.
    #[test]
    fn a_held_level_update_stamps_no_interval() {
        let at = |mic: f64| {
            recording_snapshot(&RecorderStatus {
                state: RecordingState::Recording,
                meeting_id: Some(Uuid::from_u128(1)),
                levels: Some(LaneLevels { mic, system: None }),
                ..RecorderStatus::idle()
            })
        };
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let mut sent = Sent::default();
        assert_eq!(sent.wait(&at(0.1), false, ms(0)), None, "the first goes");
        sent.went_out(at(0.1), ms(0));

        // Held at 10 ms and again at 30 ms: neither stamps.
        assert_eq!(
            sent.wait(&at(0.2), false, ms(10)),
            Some(Duration::from_millis(40))
        );
        assert_eq!(
            sent.wait(&at(0.3), false, ms(30)),
            Some(Duration::from_millis(20)),
            "the interval runs from the last send, not from a held update"
        );
        assert_eq!(sent.last.as_ref().unwrap().0, at(0.1), "held, not sent");
        // When the interval ends, the latest goes: the two folded into one.
        assert_eq!(sent.wait(&at(0.3), false, ms(50)), None);
        sent.went_out(at(0.3), ms(50));

        // A state change, a message and a resend go at once.
        let mut stopping = at(0.3);
        stopping.state = RecordingState::Stopping;
        assert_eq!(sent.wait(&stopping, false, ms(51)), None);
        let mut warned = at(0.4);
        warned.warning = Some("The disk is nearly full.".to_owned());
        assert_eq!(sent.wait(&warned, false, ms(52)), None);
        assert_eq!(sent.wait(&at(0.4), true, ms(53)), None);

        // Through the feed: a level update 30 ms after a send is held, and
        // goes out once the interval since that send ends. Had the hold
        // stamped the interval, each wake would hold it again.
        let (host, fakes, sink) = host();
        host.page_ready().unwrap();
        sink.nth_recording(1, "page.ready's snapshot");
        thread::sleep(2 * RECORDING_INTERVAL);
        recording_with(&fakes, 0.1);
        host.recorder_changed();
        let (first, _) = sink.nth_recording(2, "the first level");
        thread::sleep(RECORDING_INTERVAL * 3 / 5);
        recording_with(&fakes, 0.2);
        host.recorder_changed();
        let (held, level) = sink.nth_recording(3, "the held level");
        assert_eq!(level["level"]["mic"], 0.2);
        let after = held.duration_since(first);
        assert!(
            after < RECORDING_INTERVAL + LIVE,
            "the held level went out {after:?} after the send before it"
        );
    }

    /// P40 check 18: a command that reaches the host before `page.ready`
    /// (the page sends `page.ready` without awaiting it) loses no snapshot:
    /// once `page.ready` arrives, every topic, `recording` included,
    /// reaches the sink within [`LIVE`], with the command's effect.
    #[test]
    fn a_command_before_page_ready_loses_no_snapshot() {
        let (host, _fakes, sink) = host();
        host.recording_start(StartRecordingParams {
            mode: CaptureMode::InPerson,
        })
        .unwrap();
        host.meetings_set_filter(SetFilterParams {
            filter: ListFilter::Ready,
        })
        .unwrap();
        assert!(sink.topics().is_empty(), "nothing before page.ready");

        let ready = Instant::now();
        host.page_ready().unwrap();
        let (at, recording) = sink.nth_recording(1, "recording after page.ready");
        let took = at.saturating_duration_since(ready);
        assert!(
            took < LIVE,
            "recording reached the sink {took:?} after page.ready"
        );
        assert_eq!(recording["state"], "recording");
        let topics = sink.topics();
        for topic in TOPICS {
            assert!(topics.contains(&topic), "{topic:?} reached the sink");
        }
        let events = sink.0.lock().unwrap();
        let list = events
            .iter()
            .rev()
            .find(|(_, event)| event.topic == BridgeTopic::MeetingsList)
            .map(|(_, event)| event.payload.clone())
            .unwrap();
        assert_eq!(list["filter"], "ready", "{list}");
    }

    /// A source of the feed's own, with an idle recorder that never
    /// moves, whose drop wakes the thread as the host's does.
    struct Probe {
        feed: RecordingFeed,
        sink: Arc<Sink>,
    }

    impl Source for Probe {
        fn feed(&self) -> &RecordingFeed {
            &self.feed
        }

        fn status(&self) -> RecorderStatus {
            RecorderStatus::idle()
        }

        fn sink(&self) -> Option<Arc<dyn EventSink>> {
            Some(self.sink.clone())
        }

        fn moved(&self) {}
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.feed.wake();
        }
    }

    /// P40 check 21, the interleaving behind "once per `page.ready`": the
    /// feed's first pass (a poke is owed from the start) is paused after
    /// its swaps and before its send check, and `page.ready` lands there.
    /// The pass began before the page was ready, so it leaves "resend" to
    /// the next pass, and exactly one `recording` goes out.
    #[test]
    fn a_page_ready_during_the_first_pass_sends_the_recording_once() {
        // A lock and a condition variable, not a channel: a channel's
        // blocking receive parks the thread and would take the unpark
        // `page.ready` leaves for the next pass.
        #[derive(Default)]
        struct Step(Mutex<u8>, std::sync::Condvar);
        impl Step {
            fn to(&self, step: u8) {
                *self.0.lock().unwrap() = step;
                self.1.notify_all();
            }
            fn until(&self, step: u8) {
                let waited = self
                    .1
                    .wait_timeout_while(self.0.lock().unwrap(), PATIENCE, |at| *at < step)
                    .unwrap()
                    .1;
                assert!(!waited.timed_out(), "waited {PATIENCE:?} for step {step}");
            }
        }
        let step = Arc::new(Step::default());
        let feed = RecordingFeed::new();
        *feed.pause.lock().unwrap() = Some(Box::new({
            let step = step.clone();
            move || {
                step.to(1);
                step.until(2);
            }
        }));
        let sink = Arc::new(Sink::default());
        let probe = Arc::new(Probe {
            feed,
            sink: sink.clone(),
        });
        RecordingFeed::start(&probe, (RecordingState::Idle, None)).unwrap();
        step.until(1);
        probe.feed.page_did_become_ready();
        step.to(2);

        sink.nth_recording(1, "page.ready's snapshot");
        thread::sleep(4 * RECORDING_INTERVAL);
        assert_eq!(sink.recordings().len(), 1, "once per page.ready");
        let thread = probe.feed.take_thread().unwrap();
        drop(probe);
        found("the feed's thread ended", || {
            thread.is_finished().then_some(())
        });
    }

    /// The feed's thread ends once the last clone of the host is gone:
    /// the host's drop wakes it, and it holds the host only for a pass,
    /// so the host's own reader closes too. The thread is idle and owed
    /// no wake when the last clone goes, so only the drop's wake ends it.
    #[test]
    fn the_feed_ends_with_the_host() {
        let now = steno_core::json::parse_date("2026-09-29T12:50:00.000Z").unwrap();
        let fakes = FakeServices::new(now);
        let store = Arc::new(Store::in_memory().unwrap());
        let host = Host::new(store.clone(), fakes.services(), HostConfig::default()).unwrap();
        let clone = host.clone();
        let thread = host.take_feed_thread().unwrap();
        thread::sleep(2 * RECORDING_INTERVAL);
        drop(host);
        thread::sleep(2 * RECORDING_INTERVAL);
        assert!(!thread.is_finished(), "a clone of the host is left");
        drop(clone);
        found("the feed's thread ended", || {
            thread.is_finished().then_some(())
        });
        store
            .with_own_reader()
            .expect("the host's reader closed with the host");
    }

    /// Waits up to [`PATIENCE`] for `probe` to find `what`.
    fn found<T>(what: &str, probe: impl Fn() -> Option<T>) -> T {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(found) = probe() {
                return found;
            }
            assert!(Instant::now() < deadline, "waited {PATIENCE:?} for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }
}
