//! Swift's `DetectionTests`: the controller's decisions as a table over a
//! fake recorder and a fake panel, then the detector's events through the
//! real `MeetingDetector` over a fake process list and a `ManualClock`,
//! and the prompt's Record into the capture recorder. No sleeps: the clock
//! is advanced by hand and the tests wait for its sleepers or for the
//! panel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steno_audio::testing::{FakeProcessAudioActivity, ManualClock};
use steno_audio::{MeetingDetector, MeetingEvent, ProcessAudioActivity};
use steno_bridge::{CaptureMode, RecordingState};
use steno_core::{MeetingSource, Store};

use super::*;
use crate::auto_stop::MicrophoneActivity;
use crate::testing::{PATIENCE, SyntheticRecorder, on_own_thread, synthetic_recorder, temp_store};

#[derive(Default)]
struct FakeRecorder {
    busy: AtomicBool,
    starts: Mutex<Vec<Option<String>>>,
    activity: Mutex<Vec<MicrophoneActivity>>,
}

impl CallRecorder for FakeRecorder {
    fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn start_call(&self, call_app: Option<&str>) {
        self.starts
            .lock()
            .unwrap()
            .push(call_app.map(str::to_owned));
    }

    fn microphone_activity(&self, activity: MicrophoneActivity) {
        self.activity.lock().unwrap().push(activity);
    }
}

impl FakeRecorder {
    fn starts(&self) -> Vec<Option<String>> {
        self.starts.lock().unwrap().clone()
    }

    fn activity(&self) -> Vec<MicrophoneActivity> {
        self.activity.lock().unwrap().clone()
    }
}

/// Every call the panel got, in order.
#[derive(Default)]
struct FakePanel {
    shown: Mutex<Vec<Option<DetectionPrompt>>>,
}

impl PromptPanel for FakePanel {
    fn show(&self, prompt: Option<&DetectionPrompt>) {
        self.shown.lock().unwrap().push(prompt.cloned());
    }
}

impl FakePanel {
    fn shown(&self) -> Vec<Option<DetectionPrompt>> {
        self.shown.lock().unwrap().clone()
    }

    /// What the panel shows now.
    fn current(&self) -> Option<DetectionPrompt> {
        self.shown.lock().unwrap().last().cloned().flatten()
    }

    /// Waits up to [`PATIENCE`] for `done` to hold over what it was shown.
    fn wait_until(&self, what: &str, done: impl Fn(&[Option<DetectionPrompt>]) -> bool) {
        let deadline = std::time::Instant::now() + PATIENCE;
        while !done(&self.shown()) {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    activity: FakeProcessAudioActivity,
    clock: Arc<ManualClock>,
    recorder: Arc<FakeRecorder>,
    panel: Arc<FakePanel>,
    controller: Arc<DetectionController>,
}

/// The app names the tests read: the bundle id itself, `?` without one.
fn test_names() -> AppNames {
    Arc::new(|bundle_id| bundle_id.unwrap_or("?").to_owned())
}

fn detector(activity: &FakeProcessAudioActivity, clock: &Arc<ManualClock>) -> MeetingDetector {
    MeetingDetector::new(
        Arc::new(activity.clone()),
        clock.clone(),
        Some(std::collections::BTreeSet::new()),
        MeetingDetector::DEFAULT_DEBOUNCE,
        MeetingDetector::DEFAULT_POLL_INTERVAL,
    )
}

fn harness_over(recorder: Arc<dyn CallRecorder>, fake: Arc<FakeRecorder>) -> Harness {
    let (dir, store) = temp_store();
    let activity = FakeProcessAudioActivity::new(Vec::new());
    let clock = Arc::new(ManualClock::new());
    let panel = Arc::new(FakePanel::default());
    let controller = DetectionController::new(DetectionParts {
        detector: detector(&activity, &clock),
        recorder,
        panel: panel.clone(),
        store: store.clone(),
        clock: clock.clone(),
        app_names: test_names(),
    });
    Harness {
        _dir: dir,
        store,
        activity,
        clock,
        recorder: fake,
        panel,
        controller,
    }
}

fn harness() -> Harness {
    let recorder = Arc::new(FakeRecorder::default());
    harness_over(recorder.clone(), recorder)
}

/// A harness over `capture`'s recorder, whose changes reach the
/// controller as `App::launch` routes them. Declared after `capture`, so
/// the controller stops before the capture waits for its pipeline.
fn harness_over_capture(capture: &SyntheticRecorder) -> Harness {
    let harness = harness_over(capture.recorder.clone(), Arc::new(FakeRecorder::default()));
    let hooked = Arc::downgrade(&harness.controller);
    capture.recorder.on_change(Arc::new(move || {
        if let Some(controller) = hooked.upgrade() {
            controller.recorder_changed();
        }
    }));
    harness
}

fn opened(bundle_id: Option<&str>) -> MeetingEvent {
    MeetingEvent::MicrophoneOpened {
        bundle_id: bundle_id.map(str::to_owned),
        pid: 42,
    }
}

fn prompt(app_name: &str) -> DetectionPrompt {
    DetectionPrompt {
        app_name: app_name.to_owned(),
        seconds: PROMPT_SECONDS,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.controller.stop();
    }
}

/// Swift's `testControllerSuppressesWhileRecordingAndWhenDisabled`, one
/// row per state: an opened microphone prompts only when detection is on,
/// nothing records and no prompt is up; while recording it goes to the
/// recorder instead.
#[test]
fn an_opened_microphone_prompts_only_when_on_idle_and_no_prompt_is_up() {
    #[allow(clippy::struct_excessive_bools, reason = "a row of a table")]
    struct Row {
        what: &'static str,
        enabled: bool,
        busy: bool,
        prompt_up: bool,
        prompts: bool,
        forwards: bool,
    }
    let rows = [
        Row {
            what: "on, idle: the prompt",
            enabled: true,
            busy: false,
            prompt_up: false,
            prompts: true,
            forwards: false,
        },
        Row {
            what: "off: no prompt",
            enabled: false,
            busy: false,
            prompt_up: false,
            prompts: false,
            forwards: false,
        },
        Row {
            what: "recording: the recorder hears of it, no prompt",
            enabled: true,
            busy: true,
            prompt_up: false,
            prompts: false,
            forwards: true,
        },
        Row {
            what: "recording with detection off: the recorder still hears of it",
            enabled: false,
            busy: true,
            prompt_up: false,
            prompts: false,
            forwards: true,
        },
        Row {
            what: "a prompt up: one prompt at a time",
            enabled: true,
            busy: false,
            prompt_up: true,
            prompts: false,
            forwards: false,
        },
    ];
    for row in rows {
        let harness = harness();
        harness.controller.set_enabled(row.enabled || row.prompt_up);
        if row.prompt_up {
            harness.controller.handle(opened(Some("us.zoom.xos")));
        }
        harness.controller.set_enabled(row.enabled);
        harness.recorder.busy.store(row.busy, Ordering::SeqCst);
        let before = harness.panel.shown().len();
        harness.controller.handle(opened(Some("com.example.meet")));
        let raised = harness.panel.shown()[before..].contains(&Some(prompt("com.example.meet")));
        assert_eq!(raised, row.prompts, "{}: prompted", row.what);
        assert_eq!(
            !harness.recorder.activity().is_empty(),
            row.forwards,
            "{}: forwarded",
            row.what
        );
        if row.prompt_up {
            assert_eq!(
                harness.panel.current(),
                Some(prompt("us.zoom.xos")),
                "{}: the first prompt stays",
                row.what
            );
        }
    }
}

/// Every way a prompt goes: a release, a recording starting, detection
/// turned off, the X and Record. Each takes the prompt down once, and the
/// next opened microphone prompts again.
#[test]
fn every_way_a_prompt_goes_takes_it_down_and_frees_the_slot() {
    #[derive(Debug, Clone, Copy)]
    enum Close {
        Released,
        RecordingStarted,
        TurnedOff,
        Dismissed,
        Record,
    }
    for close in [
        Close::Released,
        Close::RecordingStarted,
        Close::TurnedOff,
        Close::Dismissed,
        Close::Record,
    ] {
        let harness = harness();
        harness.controller.set_enabled(true);
        harness.controller.handle(opened(Some("us.zoom.xos")));
        assert!(harness.controller.has_prompt(), "{close:?}: raised");
        assert!(
            harness.clock.wait_for_sleepers(2),
            "{close:?}: the poll and the prompt's countdown sleep"
        );
        match close {
            Close::Released => harness.controller.handle(MeetingEvent::MicrophoneReleased),
            Close::RecordingStarted => {
                harness.recorder.busy.store(true, Ordering::SeqCst);
                harness.controller.recorder_changed();
            }
            Close::TurnedOff => harness.controller.set_enabled(false),
            Close::Dismissed => harness.controller.dismissed(),
            Close::Record => harness.controller.record(),
        }
        assert!(!harness.controller.has_prompt(), "{close:?}: closed");
        // The panel took itself down for its own X and Record.
        let by_panel = matches!(close, Close::Dismissed | Close::Record);
        let expected = if by_panel {
            vec![Some(prompt("us.zoom.xos"))]
        } else {
            vec![Some(prompt("us.zoom.xos")), None]
        };
        assert_eq!(harness.panel.shown(), expected, "{close:?}: the panel");
        let sleepers = usize::from(!matches!(close, Close::TurnedOff));
        assert!(
            harness.clock.wait_for_sleepers(sleepers),
            "{close:?}: the countdown is withdrawn"
        );
        harness.clock.advance(Duration::from_secs(PROMPT_SECONDS));
        assert_eq!(
            harness.panel.shown().len(),
            expected.len(),
            "{close:?}: no timeout after it closed"
        );
        let starts = harness.recorder.starts();
        assert_eq!(
            starts,
            if matches!(close, Close::Record) {
                vec![Some("us.zoom.xos".to_owned())]
            } else {
                vec![]
            },
            "{close:?}: only Record starts a recording"
        );
        harness.recorder.busy.store(false, Ordering::SeqCst);
        harness.controller.set_enabled(true);
        harness.controller.handle(opened(Some("us.zoom.xos")));
        assert!(
            harness.controller.has_prompt(),
            "{close:?}: the slot is free"
        );
    }
}

#[test]
fn the_prompt_times_out_after_its_sixty_seconds() {
    let harness = harness();
    harness.controller.set_enabled(true);
    harness.controller.handle(opened(Some("us.zoom.xos")));
    // The detector's poll and the prompt's countdown.
    assert!(harness.clock.wait_for_sleepers(2));
    harness.clock.advance(Duration::from_millis(59_500));
    assert!(harness.controller.has_prompt(), "up for the full minute");
    assert!(harness.clock.wait_for_sleepers(2));
    harness.clock.advance(Duration::from_millis(500));
    harness
        .panel
        .wait_until("the prompt timed out", |shown| shown.last() == Some(&None));
    assert!(!harness.controller.has_prompt());
    harness.controller.record();
    assert_eq!(
        harness.recorder.starts(),
        Vec::<Option<String>>::new(),
        "a Record after the timeout starts nothing"
    );
}

/// Swift's `testEventsWhileRecordingReachTheRecorder`: both events reach
/// the recorder with the name resolved while it records, nothing while
/// idle, and the prompt's Record names its app (none without a bundle id).
#[test]
fn events_while_recording_reach_the_recorder_with_the_name_resolved() {
    let harness = harness();
    harness.controller.set_enabled(true);
    harness.controller.handle(opened(Some("us.zoom.xos")));
    assert!(harness.controller.has_prompt());
    assert_eq!(harness.recorder.activity(), []);
    harness.controller.record();
    assert_eq!(harness.recorder.starts(), [Some("us.zoom.xos".to_owned())]);

    harness.recorder.busy.store(true, Ordering::SeqCst);
    harness.controller.handle(opened(Some("us.zoom.xos")));
    assert!(
        !harness.controller.has_prompt(),
        "no prompt while recording"
    );
    harness.controller.handle(MeetingEvent::MicrophoneReleased);
    harness.controller.handle(opened(None));
    assert_eq!(
        harness.recorder.activity(),
        [
            MicrophoneActivity::Opened {
                app_name: Some("us.zoom.xos".to_owned())
            },
            MicrophoneActivity::Released,
            MicrophoneActivity::Opened { app_name: None },
        ]
    );

    harness.recorder.busy.store(false, Ordering::SeqCst);
    harness.controller.handle(MeetingEvent::MicrophoneReleased);
    assert_eq!(harness.recorder.activity().len(), 3, "nothing while idle");
    harness.controller.handle(opened(None));
    assert_eq!(harness.panel.current(), Some(prompt("?")));
    harness.controller.record();
    assert_eq!(
        harness.recorder.starts()[1],
        None,
        "a prompt that could not name the app attributes nothing"
    );
}

/// Swift's `testSettingsChangesFollowThroughToTheDetector`, and a detector
/// that cannot start yet (`PipeWire` not up at login) is started by the
/// next read of the setting.
#[test]
fn the_setting_starts_and_stops_the_detector_and_a_failed_start_is_retried() {
    let harness = harness();
    harness.controller.follow_settings();
    assert!(harness.controller.is_enabled(), "the default setting is on");
    assert!(harness.controller.is_detecting());
    harness.controller.handle(opened(Some("us.zoom.xos")));
    assert!(harness.controller.has_prompt());

    let mut settings = harness.store.settings().unwrap();
    settings.meeting_detection_enabled = false;
    harness.store.save_settings(&settings).unwrap();
    harness.controller.follow_settings();
    assert!(!harness.controller.is_enabled());
    assert!(!harness.controller.is_detecting(), "the detector stops");
    assert!(
        !harness.controller.has_prompt(),
        "turning it off takes the prompt down"
    );
    harness.controller.handle(opened(Some("us.zoom.xos")));
    assert!(!harness.controller.has_prompt());

    harness.activity.set_failure(Some("PipeWire is not up"));
    settings.meeting_detection_enabled = true;
    harness.store.save_settings(&settings).unwrap();
    harness.controller.follow_settings();
    assert!(harness.controller.is_enabled());
    assert!(!harness.controller.is_detecting(), "the start failed");
    harness.activity.set_failure(None);
    harness.controller.follow_settings();
    assert!(harness.controller.is_detecting(), "and is tried again");

    harness.controller.stop();
    assert!(!harness.controller.is_detecting(), "stop ends the detector");
    // A read of the setting that was under way when the app quit.
    harness.controller.follow_settings();
    assert!(
        !harness.controller.is_detecting(),
        "and nothing starts it again"
    );
    assert!(!harness.controller.is_enabled());
}

const ZOOM: i32 = 4242;

fn zoom() -> ProcessAudioActivity {
    ProcessAudioActivity::new(ZOOM, Some("us.zoom.xos"), true)
}

/// Moves the clock by `seconds` once `sleepers` sleep on it.
#[track_caller]
fn advance(harness: &Harness, sleepers: usize, seconds: f64) {
    assert!(
        harness.clock.wait_for_sleepers(sleepers),
        "{sleepers} sleepers, found {}",
        harness.clock.pending_sleepers()
    );
    harness.clock.advance(Duration::from_secs_f64(seconds));
}

/// Swift's `testDetectorEventsReachTheControllerAfterTheDebounce`, through
/// the real detector: the prompt comes after the 2 s debounce, goes with
/// the release's, and a blip shorter than the debounce never prompts.
#[test]
fn the_detector_s_events_reach_the_controller_after_the_debounce() {
    let harness = harness();
    harness.controller.set_enabled(true);
    // t = 0.5: Zoom opens the microphone; the poll sleeps until 1.0.
    advance(&harness, 1, 0.5);
    harness.activity.set(vec![zoom()]);
    // The poll and the debounce, due at 2.5.
    advance(&harness, 2, 1.5);
    assert!(
        !harness.controller.has_prompt(),
        "nothing inside the debounce"
    );
    advance(&harness, 2, 0.5);
    harness
        .panel
        .wait_until("the prompt after the debounce", |shown| {
            shown == [Some(prompt("us.zoom.xos"))]
        });

    // t = 2.75: Zoom lets go; the release's debounce ends at 4.75.
    advance(&harness, 2, 0.25);
    harness.activity.set(Vec::new());
    advance(&harness, 3, 1.5);
    assert!(
        harness.controller.has_prompt(),
        "up until the release's debounce"
    );
    advance(&harness, 3, 0.5);
    harness
        .panel
        .wait_until("the release took the prompt down", |shown| {
            shown.last() == Some(&None)
        });

    assert!(
        harness.clock.wait_for_sleepers(1),
        "the poll alone, the countdown withdrawn"
    );
    harness.activity.set(vec![zoom()]);
    assert!(
        harness.clock.wait_for_sleepers(2),
        "the debounce armed again"
    );
    harness.activity.set(Vec::new());
    assert!(harness.clock.wait_for_sleepers(1), "and was forgotten");
    for _ in 0..5 {
        advance(&harness, 1, 1.0);
    }
    assert_eq!(harness.panel.shown().len(), 2, "a blip is not a call");
}

/// After a relaunch (an update's, a crash's) during a call, the microphone
/// is already held when the detector starts: it prompts after the debounce
/// like any newly opened one. The stable plan's S2 relies on it for a
/// Record the update install refused.
#[test]
fn a_call_already_under_way_at_launch_prompts() {
    let harness = harness();
    harness.activity.set(vec![zoom()]);
    harness.controller.follow_settings();
    advance(&harness, 2, 2.0);
    harness
        .panel
        .wait_until("the call under way prompts", |shown| {
            shown == [Some(prompt("us.zoom.xos"))]
        });
}

/// Swift's `testStopDuringAStillOpenMicrophoneDoesNotRePrompt`, over the
/// capture recorder: the prompt's Record starts a call recording for Zoom;
/// a Stop while Zoom still holds the microphone prompts no more, and a
/// microphone released and opened again does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prompt_records_the_call_and_a_stop_during_it_does_not_prompt_again() {
    let capture = synthetic_recorder();
    let harness = harness_over_capture(&capture);
    harness.controller.set_enabled(true);
    advance(&harness, 1, 0.5);
    harness.activity.set(vec![zoom()]);
    advance(&harness, 2, 2.0);
    harness
        .panel
        .wait_until("Zoom prompts", |shown| !shown.is_empty());

    let controller = harness.controller.clone();
    on_own_thread(PATIENCE, "Record returned", move || controller.record());
    let status = capture.recorder.status();
    assert_eq!(status.state, RecordingState::Recording);
    assert_eq!(status.mode, Some(CaptureMode::Call));
    assert_eq!(status.call_app.as_deref(), Some("us.zoom.xos"));
    let meeting = capture.store.meetings(10, 0).unwrap().remove(0);
    assert_eq!(meeting.source, MeetingSource::MacCall, "a call's source");

    // A second Record, a stale click, starts nothing more.
    let controller = harness.controller.clone();
    on_own_thread(PATIENCE, "Record returned", move || controller.record());
    let recorder = capture.recorder.clone();
    on_own_thread(PATIENCE, "Stop returned", move || recorder.stop());
    assert_eq!(
        capture.store.meetings(10, 0).unwrap().len(),
        1,
        "one recording"
    );

    // Five polls and more than a debounce with Zoom still on the line.
    for _ in 0..5 {
        advance(&harness, 1, 1.0);
    }
    assert_eq!(harness.panel.shown().len(), 1, "no second prompt");

    harness.activity.set(Vec::new());
    advance(&harness, 2, 2.0);
    // The release reported, else the next open falls inside its debounce.
    let deadline = std::time::Instant::now() + PATIENCE;
    while harness.controller.detector.holder().is_some() {
        assert!(std::time::Instant::now() < deadline, "the release");
        std::thread::sleep(Duration::from_millis(1));
    }
    harness.activity.set(vec![zoom()]);
    advance(&harness, 2, 2.0);
    harness
        .panel
        .wait_until("a microphone opened anew prompts again", |shown| {
            shown.len() == 2
        });
}

/// A recording started from the main window while the prompt is up takes
/// the prompt down, and the prompt's Record, should a click still land,
/// does not start a second recording.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prompt_never_starts_a_second_recording() {
    let capture = synthetic_recorder();
    let harness = harness_over_capture(&capture);
    harness.controller.set_enabled(true);
    harness.controller.handle(opened(Some("us.zoom.xos")));
    assert!(harness.controller.has_prompt());
    let recorder = capture.recorder.clone();
    on_own_thread(PATIENCE, "the start returned", move || {
        recorder.start(CaptureMode::InPerson, None);
    });
    assert!(
        !harness.controller.has_prompt(),
        "the recording took it down"
    );
    let controller = harness.controller.clone();
    on_own_thread(PATIENCE, "Record returned", move || controller.record());
    let meetings = capture.store.meetings(10, 0).unwrap();
    assert_eq!(meetings.len(), 1);
    assert_eq!(meetings[0].source, MeetingSource::MacInPerson);
}

#[test]
fn an_app_without_a_bundle_id_is_another_app() {
    assert_eq!(fallback_app_name(None), "Another app");
    assert_eq!(fallback_app_name(Some("firefox")), "firefox");
}

/// A process list whose snapshots wait while its gate is closed, as a
/// detector's start waits while `PipeWire` reconnects at logout.
struct GatedActivity {
    /// Whether the gate is closed, and how many snapshots wait at it.
    gate: Mutex<(bool, usize)>,
    changed: std::sync::Condvar,
    listeners: Mutex<Vec<std::sync::mpsc::Sender<()>>>,
}

impl GatedActivity {
    fn closed() -> Arc<Self> {
        Arc::new(Self {
            gate: Mutex::new((true, 0)),
            changed: std::sync::Condvar::new(),
            listeners: Mutex::default(),
        })
    }

    /// Waits until a snapshot waits at the gate.
    fn until_held(&self) {
        let (gate, timeout) = self
            .changed
            .wait_timeout_while(self.gate.lock().unwrap(), PATIENCE, |(_, held)| *held == 0)
            .unwrap();
        drop(gate);
        assert!(!timeout.timed_out(), "a snapshot waits at the gate");
    }

    fn open(&self) {
        self.gate.lock().unwrap().0 = false;
        self.changed.notify_all();
    }
}

impl steno_audio::ProcessAudioActivitySource for GatedActivity {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, steno_audio::detection::ActivityError> {
        let mut gate = self.gate.lock().unwrap();
        gate.1 += 1;
        self.changed.notify_all();
        let mut gate = self
            .changed
            .wait_while(gate, |(closed, _)| *closed)
            .unwrap();
        gate.1 -= 1;
        Ok(Vec::new())
    }

    fn changes(&self) -> std::sync::mpsc::Receiver<()> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.listeners.lock().unwrap().push(sender);
        receiver
    }
}

/// A controller over `recorder` whose detector reads `activity`.
fn gated_controller(
    recorder: Arc<dyn CallRecorder>,
    store: Arc<Store>,
    activity: Arc<GatedActivity>,
) -> (Arc<DetectionController>, Arc<FakePanel>) {
    let clock = Arc::new(ManualClock::new());
    let panel = Arc::new(FakePanel::default());
    let controller = DetectionController::new(DetectionParts {
        detector: MeetingDetector::new(
            activity,
            clock.clone(),
            Some(std::collections::BTreeSet::new()),
            MeetingDetector::DEFAULT_DEBOUNCE,
            MeetingDetector::DEFAULT_POLL_INTERVAL,
        ),
        recorder,
        panel: panel.clone(),
        store,
        clock,
        app_names: test_names(),
    });
    (controller, panel)
}

/// The stop turns detection off and takes the prompt down at once, though
/// a detector start still waits on its first snapshot; only the
/// detector's own stop waits for that.
#[test]
fn a_stop_takes_the_prompt_down_while_a_detector_start_still_waits() {
    let (_dir, store) = temp_store();
    let activity = GatedActivity::closed();
    let (controller, panel) =
        gated_controller(Arc::new(FakeRecorder::default()), store, activity.clone());
    let starting = {
        let controller = controller.clone();
        std::thread::spawn(move || controller.set_enabled(true))
    };
    activity.until_held();
    controller.handle(opened(Some("us.zoom.xos")));
    assert!(controller.has_prompt(), "on while the start waits");
    let stopping = {
        let controller = controller.clone();
        std::thread::spawn(move || controller.stop())
    };
    panel.wait_until("the stop took the prompt down", |shown| {
        shown.last() == Some(&None)
    });
    assert!(!controller.is_enabled(), "off before the start returned");
    controller.handle(opened(Some("com.example.meet")));
    assert!(!controller.has_prompt(), "and no event raises another");
    activity.open();
    starting.join().unwrap();
    stopping.join().unwrap();
    assert!(!controller.is_detecting(), "the detector stopped after all");
}

/// Swift's quit saved the recording first. A detector start held in its
/// snapshot (`PipeWire` reconnecting at logout) does not hold up the quit
/// save of a call recording: `App::shutdown` stops detection last.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quit_save_does_not_wait_for_a_held_detector_start() {
    use steno_core::RecordingEndReason;
    let (dir, store) = temp_store();
    let mut app =
        crate::testing::app_over_fakes(dir.path(), &store, crate::testing::synthetic_capture());
    let activity = GatedActivity::closed();
    let (controller, _panel) =
        gated_controller(app.recorder.clone(), store.clone(), activity.clone());
    app.detection = Some(controller.clone());
    let app = Arc::new(app);
    let recorder = app.recorder.clone();
    on_own_thread(PATIENCE, "the start returned", move || {
        recorder.start(CaptureMode::Call, None);
    });
    let meeting_id = app.recorder.status().meeting_id.expect("recording");
    let starting = {
        let controller = controller.clone();
        std::thread::spawn(move || controller.set_enabled(true))
    };
    activity.until_held();
    let quitting = {
        let app = app.clone();
        std::thread::spawn(move || app.shutdown())
    };
    crate::testing::eventually("the quit saved the call while the start is held", || {
        store
            .meeting(meeting_id)
            .unwrap()
            .is_some_and(|meeting| meeting.end_reason == Some(RecordingEndReason::Quit))
    })
    .await;
    assert_eq!(app.recorder.status().state, RecordingState::Idle);
    activity.open();
    tokio::task::block_in_place(|| {
        starting.join().unwrap();
        quitting.join().unwrap();
    });
    assert!(!controller.is_detecting(), "the shutdown stopped detection");
    app.pipeline.current().wait_until_idle().await;
}
