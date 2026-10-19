//! The `recording` topic's feed: one thread that reads the recorder's
//! status and sends the snapshot with no host lock taken, so the levels,
//! `Stopping` and a stop's hook never wait for a reload, a command or a
//! publish.
//!
//! - **A poke** sets "poked", unparks the thread and returns.
//! - **Each pass** reads "page ready", swaps "poked" (and "resend" once the
//!   page is ready), reads `status()`, and sends only if the page was ready
//!   when the pass began. A snapshot goes out at once on "resend" or when
//!   anything but its levels changed; otherwise it waits out the rest of
//!   [`RECORDING_INTERVAL`], so every hook still gets its snapshot. Only a
//!   snapshot that went out stamps the interval.
//! - **A move** of the recorder's state or meeting asks the reload owner
//!   for a pass ([`Host::request_reload`](crate::Host::request_reload)),
//!   since the list tells a live row from one left `recording` by the
//!   recorder's state.
//! - **`page.ready`** sets "resend", then "page ready", then unparks: every
//!   `page.ready` gets the current `recording` exactly once. A pass that
//!   finds the page ready finds "resend" set; one that began before leaves
//!   it for the next.
//!
//! Only the feed's thread touches the last snapshot sent, so it needs no
//! lock. The thread holds a `Weak` on the host and ends once no host is
//! left.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use steno_bridge::{BridgeEvent, BridgeTopic, EventSink, RecordingSnapshot, RecordingState};
use uuid::Uuid;

use crate::main_window::snapshots::recording_snapshot;
use crate::services::RecorderStatus;

/// 20 Hz: the level meter's rate on the page.
pub const RECORDING_INTERVAL: Duration = Duration::from_millis(50);

/// The recorder's state and meeting, whose move asks for a reload.
pub(crate) type Position = (RecordingState, Option<Uuid>);

/// What the feed reads and where it sends. Each method takes at most one
/// leaf lock, briefly, and none of the host's.
pub(crate) trait Source: Send + Sync {
    fn feed(&self) -> &RecordingFeed;
    fn status(&self) -> RecorderStatus;
    /// The sink attached now, the slot's lock released.
    fn sink(&self) -> Option<Arc<dyn EventSink>>;
    /// The recorder's state or meeting moved: asks the reload owner for a
    /// pass.
    fn moved(&self);
}

/// The flags other threads set, and the handle that wakes the feed.
pub(crate) struct RecordingFeed {
    page_ready: AtomicBool,
    /// The next pass sends the current snapshot, changed or not.
    resend: AtomicBool,
    /// A poke came since the last pass. Set at first, so the first pass
    /// reads the status even when a move came before the thread could be
    /// woken.
    poked: AtomicBool,
    thread: OnceLock<thread::Thread>,
    /// The thread's handle, so a test can tell that it ended.
    #[cfg(test)]
    joined: std::sync::Mutex<Option<thread::JoinHandle<()>>>,
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
        }
    }

    /// Starts the feed's thread over `source`, whose recorder stood at
    /// `seen` before the host loaded, so a move after that asks for a
    /// reload.
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

    /// Wakes the feed for a pass; takes no lock.
    pub(crate) fn poke(&self) {
        self.poked.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// `page.ready`: the next pass sends the current snapshot at once.
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

    #[cfg(test)]
    pub(crate) fn take_thread(&self) -> Option<thread::JoinHandle<()>> {
        self.joined.lock().unwrap().take()
    }
}

/// The feed's thread: a pass per wake until no host is left.
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

/// One pass, `held` when the last one held a snapshot back; returns how
/// long until the snapshot this one holds back may go out.
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
    /// How long `snapshot` must wait at `now`; `None` when it goes out now
    /// (on `resend`, the first one, or anything but the levels moved).
    fn wait(&self, snapshot: &RecordingSnapshot, resend: bool, now: Instant) -> Option<Duration> {
        let (last, at) = self.last.as_ref()?;
        if resend || without_level(last) != without_level(snapshot) {
            return None;
        }
        let left = RECORDING_INTERVAL.saturating_sub(now.saturating_duration_since(*at));
        (!left.is_zero()).then_some(left)
    }

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
    /// enough for a loaded machine.
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

        /// Waits for the `n`th `recording` snapshot; when it arrived and its
        /// payload.
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

    /// Before `page.ready` nothing reaches the sink; the first snapshot
    /// then carries the status as it is.
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

    /// With the view-model lock held, level updates and `Stopping` reach
    /// the sink within [`LIVE`], and each hook returns at once.
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

    /// A second `page.ready` gets the current `recording` once more, though
    /// nothing moved.
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

    /// The throttle: a held-back level update neither stamps the interval
    /// nor becomes the last sent, and goes out when the interval ends; two
    /// inside one interval fold into the later. Anything else goes at once.
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
    }

    /// A command before `page.ready` loses no snapshot: once `page.ready`
    /// arrives, every topic reaches the sink within [`LIVE`], with the
    /// command's effect.
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

    /// The feed's thread ends once the last clone of the host is gone, and
    /// the host's own reader closes too.
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
