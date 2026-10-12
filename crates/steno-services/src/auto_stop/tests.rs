//! Swift's `AutoStopTests`: the policy as a table over [`CallWatch`], then
//! the recorder over the synthetic capture and a `ManualClock`, its
//! meetings read from the store. No sleeps: the clock is advanced by hand
//! and the tests wait for its sleepers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use steno_audio::testing::ManualClock;
use steno_bridge::{CaptureMode, RecordingState};
use steno_core::{MeetingState, RecordingEndReason};
use steno_host::services::Recorder as _;

use super::*;
use crate::testing::{
    HANG_GUARD, PATIENCE, SyntheticRecorder, eventually, eventually_within, on_own_thread,
    synthetic_recorder, synthetic_recorder_over,
};

/// One step of a [`CallWatch`] script.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// A recording starts, from the prompt for this app or not.
    Begin(Option<&'static str>),
    Opened(Option<&'static str>),
    Released,
    /// The capture begins to recover.
    Recovering,
    Keep,
    /// The capture is back after recovering.
    Resume,
    /// Any stop.
    Reset,
    /// The clock moves on, in seconds.
    Wait(f64),
}

/// What a script leaves: the armed app (`Some(None)` for an unnamed one)
/// and the seconds the status shows, or `None` when nothing is armed.
type Armed = Option<(Option<&'static str>, f64)>;

/// The status `script` leaves: the armed app and the seconds shown.
fn run(script: &[Step]) -> Option<(Option<String>, f64)> {
    let mut watch = CallWatch::default();
    let mut now = Duration::ZERO;
    let mut recovering = false;
    for step in script {
        match *step {
            Step::Begin(app) => watch.begin(app),
            Step::Opened(app) => watch.opened(app.map(str::to_owned)),
            Step::Released => {
                watch.released(now);
            }
            Step::Recovering => recovering = true,
            Step::Keep => {
                watch.keep_recording();
            }
            Step::Resume => {
                recovering = false;
                watch.resume(now);
            }
            Step::Reset => watch.reset(),
            Step::Wait(seconds) => now += Duration::from_secs_f64(seconds),
        }
    }
    watch
        .status(now, recovering)
        .map(|status| (status.app_name, status.remaining_seconds))
}

#[test]
#[allow(clippy::too_many_lines, reason = "one row per case of the table")]
fn the_policy_arms_cancels_and_forgets_as_swift_s_did() {
    use Step::{Begin, Keep, Opened, Recovering, Released, Reset, Resume, Wait};
    let zen = Some("Zen");
    let rows: &[(&str, &[Step], Armed)] = &[
        (
            "an open microphone is a call in progress",
            &[Begin(None), Opened(zen)],
            None,
        ),
        (
            "the release after a call arms the full grace",
            &[Begin(None), Opened(zen), Released],
            Some((zen, 90.0)),
        ),
        (
            "one second later the status shows 89",
            &[Begin(None), Opened(zen), Released, Wait(1.0)],
            Some((zen, 89.0)),
        ),
        (
            "half a second in it still shows 90, as Swift's ticks did",
            &[Begin(None), Opened(zen), Released, Wait(0.5)],
            Some((zen, 90.0)),
        ),
        (
            "a second release does not arm again",
            &[Begin(None), Opened(zen), Released, Wait(10.0), Released],
            Some((zen, 80.0)),
        ),
        (
            "the microphone opened again cancels the countdown",
            &[Begin(None), Opened(zen), Released, Wait(1.0), Opened(zen)],
            None,
        ),
        (
            "the next release after it arms a fresh countdown",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Wait(30.0),
                Opened(zen),
                Released,
            ],
            Some((zen, 90.0)),
        ),
        (
            "Keep recording cancels it",
            &[Begin(None), Opened(zen), Released, Keep],
            None,
        ),
        (
            "and another release does not arm it again",
            &[Begin(None), Opened(zen), Released, Keep, Released],
            None,
        ),
        (
            "until the next call is seen",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Keep,
                Opened(Some("Meet")),
                Released,
            ],
            Some((Some("Meet"), 90.0)),
        ),
        (
            "a Keep recording that lands once the call is back changes nothing",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Keep,
                Opened(Some("Meet")),
                Keep,
                Released,
            ],
            Some((Some("Meet"), 90.0)),
        ),
        ("no call seen never arms", &[Begin(None), Released], None),
        (
            "an unnamed app arms all the same",
            &[Begin(None), Opened(None), Released],
            Some((None, 90.0)),
        ),
        (
            "a recording started from the prompt arms on the first release",
            &[Begin(zen), Released],
            Some((zen, 90.0)),
        ),
        (
            "a stop forgets the call",
            &[Begin(None), Opened(zen), Reset, Begin(None), Released],
            None,
        ),
        (
            "a stop while armed disarms",
            &[Begin(None), Opened(zen), Released, Reset],
            None,
        ),
        (
            "a new start forgets the last recording's call",
            &[Begin(None), Opened(zen), Begin(None), Released],
            None,
        ),
        (
            "the status never counts below zero",
            &[Begin(None), Opened(zen), Released, Wait(120.0)],
            Some((zen, 0.0)),
        ),
        (
            "audio back after the release arms a fresh grace",
            &[Begin(None), Opened(zen), Released, Wait(30.0), Resume],
            Some((zen, 90.0)),
        ),
        (
            "and so it does once the grace ran out while the capture recovered",
            &[Begin(None), Opened(zen), Released, Wait(120.0), Resume],
            Some((zen, 90.0)),
        ),
        (
            "audio back while the app holds the microphone arms nothing",
            &[Begin(None), Opened(zen), Resume],
            None,
        ),
        (
            "nor once the app opened it again",
            &[Begin(None), Opened(zen), Released, Opened(zen), Resume],
            None,
        ),
        (
            "nor after Keep recording",
            &[Begin(None), Opened(zen), Released, Keep, Resume],
            None,
        ),
        (
            "nor with no call seen",
            &[Begin(None), Released, Resume],
            None,
        ),
        (
            "a new start forgets the release",
            &[Begin(None), Opened(zen), Released, Begin(zen), Resume],
            None,
        ),
        (
            "a release while the capture recovers shows no countdown",
            &[Begin(None), Opened(zen), Recovering, Released],
            None,
        ),
        (
            "and the capture back arms a fresh one",
            &[
                Begin(None),
                Opened(zen),
                Recovering,
                Released,
                Wait(30.0),
                Resume,
            ],
            Some((zen, 90.0)),
        ),
        (
            "the release's countdown is not shown while the capture recovers",
            &[Begin(None), Opened(zen), Released, Wait(10.0), Recovering],
            None,
        ),
        (
            "only the release's countdown is replaced: a later resume leaves the fresh one",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Wait(10.0),
                Resume,
                Wait(30.0),
                Recovering,
                Resume,
            ],
            Some((zen, 60.0)),
        ),
        (
            "and the fresh one shows while the capture recovers",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Resume,
                Wait(30.0),
                Recovering,
            ],
            Some((zen, 60.0)),
        ),
        (
            "a stop forgets the release",
            &[
                Begin(None),
                Opened(zen),
                Released,
                Reset,
                Begin(zen),
                Resume,
            ],
            None,
        ),
    ];
    for (what, script, expected) in rows {
        let armed = run(script);
        let armed = armed
            .as_ref()
            .map(|(app, seconds)| (app.as_deref(), *seconds));
        assert_eq!(armed, *expected, "{what}");
    }
}

#[test]
fn only_the_armed_countdown_ends_the_call_with_its_app() {
    let mut watch = CallWatch::default();
    watch.begin(None);
    watch.opened(Some("Zen".to_owned()));
    let first = watch.released(Duration::ZERO).expect("armed");
    watch.opened(Some("Zen".to_owned()));
    assert!(
        first.cancel.is_cancelled(),
        "the opened microphone withdrew it"
    );
    let second = watch.released(Duration::ZERO).expect("armed again");
    assert_eq!(
        watch.elapsed(first.number),
        None,
        "a stale countdown stops nothing"
    );
    assert_eq!(
        watch.elapsed(second.number),
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
    assert_eq!(watch.status(Duration::ZERO, false), None, "and it is spent");

    let mut unnamed = CallWatch::default();
    unnamed.begin(None);
    unnamed.opened(None);
    let countdown = unnamed.released(Duration::ZERO).expect("armed");
    assert_eq!(
        unnamed.elapsed(countdown.number),
        Some(RecordingEndReason::CallEnded { app_name: None }),
        "a nameless call is stored as the bare `callEnded`"
    );
}

/// The cap on the wait for a recovering capture: the release's countdown
/// that runs out while the capture recovers is replaced by a fresh one,
/// shown at once, and that one ends the call whether or not the capture
/// still recovers; a countdown that runs out while the capture records
/// ends the call as before.
#[test]
fn the_release_s_countdown_runs_out_into_one_fresh_grace_while_the_capture_recovers() {
    let grace = AUTO_STOP_GRACE;
    let mut watch = CallWatch::default();
    watch.begin(None);
    watch.opened(Some("Zen".to_owned()));
    let first = watch.released(Duration::ZERO).expect("armed");
    assert!(
        watch.postpone(first.number, grace, false).is_none(),
        "the capture records: it ends the call"
    );
    let fresh = watch
        .postpone(first.number, grace, true)
        .expect("a fresh grace in its place");
    assert!(first.cancel.is_cancelled());
    assert_eq!(watch.elapsed(first.number), None, "the first is spent");
    let shown = watch.status(grace, true).expect("shown while recovering");
    assert_eq!(shown.remaining_seconds, 90.0);
    assert!(
        watch.postpone(fresh.number, grace * 2, true).is_none(),
        "the fresh one is not postponed"
    );
    assert!(
        watch.resume(grace * 2).is_none(),
        "nor replaced by a resume"
    );
    assert_eq!(
        watch.elapsed(fresh.number),
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
    watch.opened(Some("Zen".to_owned()));
    watch.released(grace * 2).expect("armed again");
    assert!(
        watch.postpone(first.number, grace * 2, true).is_none(),
        "a stale countdown postpones nothing"
    );
}

/// The end reason an auto-stop writes is one the Swift app decodes: the
/// `meeting.endReason` column holds `{"callEnded":"Zen"}` or the bare
/// `"callEnded"`, as `RecordingEndReason`'s coding in
/// `Sources/StenoCore/Model/Meeting.swift` writes them; no new value.
#[test]
fn the_end_reason_is_one_the_swift_app_decodes() {
    for (reason, column) in [
        (
            RecordingEndReason::CallEnded {
                app_name: Some("Zen".to_owned()),
            },
            r#"{"callEnded":"Zen"}"#,
        ),
        (
            RecordingEndReason::CallEnded { app_name: None },
            r#""callEnded""#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&reason).unwrap(), column);
    }
}

// The recorder.

/// A [`SyntheticRecorder`] whose auto-stop runs on a manual clock.
struct Recording {
    capture: SyntheticRecorder,
    clock: Arc<ManualClock>,
}

impl std::ops::Deref for Recording {
    type Target = SyntheticRecorder;

    fn deref(&self) -> &SyntheticRecorder {
        &self.capture
    }
}

fn recording() -> Recording {
    recording_over(synthetic_recorder())
}

/// A [`Recording`] over `capture`.
fn recording_over(capture: SyntheticRecorder) -> Recording {
    let clock = Arc::new(ManualClock::new());
    capture.recorder.count_down_on(clock.clone());
    Recording { capture, clock }
}

impl Recording {
    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        self.capture.start(mode, call_app);
        assert_eq!(self.recorder.status().state, RecordingState::Recording);
    }

    fn opened(&self, app: Option<&str>) {
        self.recorder
            .microphone_activity(MicrophoneActivity::Opened {
                app_name: app.map(str::to_owned),
            });
    }

    fn released(&self) {
        self.recorder
            .microphone_activity(MicrophoneActivity::Released);
    }

    /// The capture is back, on a thread of its own, so a resume that
    /// deadlocks fails the test rather than hangs it.
    fn resume(&self) {
        let recorder = self.recorder.clone();
        on_own_thread(PATIENCE, "the resume returned", move || {
            recorder.resume_auto_stop();
        });
    }

    /// The armed countdown's app and seconds, as the status shows them.
    fn armed(&self) -> Option<(Option<String>, f64)> {
        self.recorder
            .status()
            .auto_stop
            .map(|armed| (armed.app_name, armed.remaining_seconds))
    }

    /// Waits until the countdown's thread sleeps on the clock (or until
    /// none does), so an advance reaches it.
    fn sleepers(&self, count: usize) {
        assert!(
            self.clock.wait_for_sleepers(count),
            "{count} sleepers on the clock, found {}",
            self.clock.pending_sleepers()
        );
    }

    fn meetings(&self) -> Vec<steno_core::Meeting> {
        self.store.meetings(10, 0).unwrap()
    }

    /// Records, through the change hook, the countdown the status shows
    /// at every change while the recording is Stopping: what the bubble
    /// shows while a stop saves.
    fn watch_stopping(&self) -> Arc<std::sync::Mutex<Vec<Option<AutoStopStatus>>>> {
        let while_stopping = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (seen, watched) = (while_stopping.clone(), Arc::downgrade(&self.recorder));
        self.recorder.on_change(Arc::new(move || {
            if let Some(recorder) = watched.upgrade() {
                let status = recorder.status();
                if status.state == RecordingState::Stopping {
                    seen.lock().unwrap().push(status.auto_stop);
                }
            }
        }));
        while_stopping
    }
}

/// Asserts that a stop was seen and showed no countdown while it saved.
fn assert_no_countdown_while_stopping(
    while_stopping: &std::sync::Mutex<Vec<Option<AutoStopStatus>>>,
) {
    let while_stopping = while_stopping.lock().unwrap().clone();
    assert!(!while_stopping.is_empty(), "the stop was seen");
    assert!(
        while_stopping.iter().all(Option::is_none),
        "no countdown while the stop saves: {while_stopping:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_grace_elapsed_stops_and_saves_the_call_as_a_stop_does() {
    let recording = recording();
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    assert_eq!(recording.armed(), None, "a call in progress");
    assert_eq!(
        recording.recorder.status().call_app.as_deref(),
        Some("Zen"),
        "the recording is attributed to the app that opened the microphone"
    );
    recording.released();
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 90.0)));
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(1));
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 89.0)));
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(88));
    assert_eq!(
        recording.recorder.status().state,
        RecordingState::Recording,
        "still recording at 0:01"
    );
    // The auto-stop goes through Stopping as Stop does, its countdown
    // gone from the stop's start.
    let while_stopping = recording.watch_stopping();
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(1));
    recording.idle("the grace stopped the recording").await;
    let status = recording.recorder.status();
    assert_eq!(status.auto_stop, None);
    assert_eq!(status.error, None);
    assert_no_countdown_while_stopping(&while_stopping);
    let meetings = recording.meetings();
    assert_eq!(meetings.len(), 1);
    let meeting = &meetings[0];
    assert_eq!(
        meeting.end_reason,
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
    // Saved as Stop saves: the asset written, the duration set and the
    // meeting handed to processing.
    assert!(
        !matches!(
            meeting.state,
            MeetingState::Recording | MeetingState::Failed { .. }
        ),
        "handed to processing, got {:?}",
        meeting.state
    );
    assert!(meeting.duration > 0.0, "the duration is stored");
    let asset = recording
        .store
        .asset(meeting.id)
        .unwrap()
        .expect("the asset row");
    assert!(
        steno_core::paths::file_url_path(&asset.url).is_some_and(|path| path.exists()),
        "the recording is kept on disk"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unnamed_app_ends_the_call_with_the_bare_reason() {
    let recording = recording();
    recording.start(CaptureMode::Call, None);
    recording.opened(None);
    recording.released();
    assert_eq!(recording.armed(), Some((None, 90.0)));
    recording.sleepers(1);
    recording.clock.advance(AUTO_STOP_GRACE);
    recording.idle("the grace stopped the recording").await;
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::CallEnded { app_name: None })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_from_the_prompt_arms_on_its_first_release() {
    let recording = recording();
    recording.start(CaptureMode::Call, Some("Zen"));
    assert_eq!(recording.recorder.status().call_app.as_deref(), Some("Zen"));
    recording.released();
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 90.0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_recording_withdraws_the_countdown_until_the_next_call() {
    let recording = recording();
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    recording.recorder.keep_recording();
    assert_eq!(recording.armed(), None);
    recording.sleepers(0);
    recording.clock.advance(AUTO_STOP_GRACE * 2);
    recording.released();
    assert_eq!(recording.armed(), None, "another release does not arm it");
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    recording.opened(Some("Meet"));
    recording.released();
    assert_eq!(
        recording.armed(),
        Some((Some("Meet".to_owned()), 90.0)),
        "the next call arms again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_call_resuming_withdraws_the_countdown() {
    let recording = recording();
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    recording.opened(Some("Zen"));
    assert_eq!(recording.armed(), None, "the call is back on");
    recording.sleepers(0);
    recording.clock.advance(AUTO_STOP_GRACE);
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    recording.released();
    assert_eq!(
        recording.armed(),
        Some((Some("Zen".to_owned()), 90.0)),
        "the next release arms a fresh countdown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_person_recording_never_arms() {
    let recording = recording();
    recording.start(CaptureMode::InPerson, Some("FaceTime"));
    recording.opened(Some("FaceTime"));
    recording.released();
    assert_eq!(recording.armed(), None);
    assert_eq!(recording.clock.pending_sleepers(), 0);
    recording.stop();
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::Manual)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_armed_keeps_its_own_reason_and_withdraws_the_countdown() {
    for (quit, reason) in [
        (false, RecordingEndReason::Manual),
        (true, RecordingEndReason::Quit),
    ] {
        let recording = recording();
        recording.start(CaptureMode::Call, None);
        recording.opened(Some("Zen"));
        recording.released();
        recording.sleepers(1);
        // The countdown is gone from the stop's start, as Swift's stop
        // disarmed first.
        let while_stopping = recording.watch_stopping();
        if quit {
            let recorder = recording.recorder.clone();
            on_own_thread(HANG_GUARD, "the quit returned", move || {
                recorder.stop_for_quit();
            });
        } else {
            recording.stop();
        }
        assert_eq!(recording.recorder.status().state, RecordingState::Idle);
        assert_eq!(recording.armed(), None);
        assert_no_countdown_while_stopping(&while_stopping);
        recording.sleepers(0);
        recording.clock.advance(AUTO_STOP_GRACE);
        let meetings = recording.meetings();
        assert_eq!(meetings.len(), 1, "nothing else happened");
        assert_eq!(meetings[0].end_reason, Some(reason));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_forgets_the_call_and_idle_events_are_not_remembered() {
    let recording = recording();
    recording.opened(Some("Zen"));
    recording.released();
    assert_eq!(recording.armed(), None, "idle: nothing arms");
    recording.start(CaptureMode::Call, None);
    recording.released();
    assert_eq!(
        recording.armed(),
        None,
        "the idle-time open was not remembered"
    );
    recording.opened(Some("Zen"));
    recording.stop();
    recording.start(CaptureMode::Call, None);
    assert_eq!(recording.recorder.status().call_app, None);
    recording.released();
    assert_eq!(
        recording.armed(),
        None,
        "the second recording saw no call of its own"
    );
}

/// A call joined while Steno still starts is remembered: the detector
/// forwards from the moment the start is announced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_microphone_opened_while_starting_is_remembered() {
    let recording = recording();
    let watched = Arc::downgrade(&recording.recorder);
    let once = AtomicBool::new(false);
    recording.recorder.on_change(Arc::new(move || {
        // Once: the change it reports comes back here.
        if let Some(recorder) = watched.upgrade()
            && recorder.status().state == RecordingState::Starting
            && !once.swap(true, Ordering::SeqCst)
        {
            recorder.microphone_activity(MicrophoneActivity::Opened {
                app_name: Some("Zen".to_owned()),
            });
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.released();
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 90.0)));
}

/// A recording whose device changes a second into it: the capture
/// rebuilds on the new one and its notice thread calls the change hook.
fn recording_whose_device_changes() -> Recording {
    recording_over(synthetic_recorder_over(Arc::new(|configuration| {
        let options = crate::testing::synthetic_tone(&configuration).change_device_after(1.0);
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })))
}

/// Whether the caller is the capture's notice thread.
fn on_the_notice_thread() -> bool {
    std::thread::current().name() == Some("steno-notices")
}

/// Swift's `testADeviceChangeNoticeLeavesTheCountdownRunning`, which
/// differs in Rust: a device change during the countdown hides it until
/// the capture resumes on the new device, and the resume arms a fresh
/// 90 s (the capture recovered, [`CaptureRecorder::resume_auto_stop`])
/// that still ends the call.
///
/// [`CaptureRecorder::resume_auto_stop`]: crate::recorder::CaptureRecorder::resume_auto_stop
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_change_rearms_the_countdown_at_its_resume() {
    // Not dropped until the resume was seen: the change hook reads the
    // status on the notice thread, so a notice handled under the outage's
    // lock deadlocks there, and the drop's stop would wait on it.
    let recording = std::mem::ManuallyDrop::new(recording_whose_device_changes());
    let (changed, rebuilt) = std::sync::mpsc::channel();
    let watched = Arc::downgrade(&recording.recorder);
    recording.recorder.on_change(Arc::new(move || {
        if on_the_notice_thread()
            && let Some(recorder) = watched.upgrade()
        {
            let _ = changed.send(
                recorder
                    .status()
                    .auto_stop
                    .map(|armed| armed.remaining_seconds),
            );
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(10));
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 80.0)));
    // The fresh countdown on a clock of its own, at the same time, so
    // each clock's sleepers are its own countdown's.
    let resumed = Arc::new(ManualClock::new());
    resumed.advance(Duration::from_secs(10));
    recording.recorder.count_down_on(resumed.clone());
    let armed_then = rebuilt.recv_timeout(PATIENCE).expect("the device changed");
    assert_eq!(armed_then, None, "not shown while the capture recovers");
    while rebuilt.recv_timeout(PATIENCE).expect("the capture resumed") != Some(90.0) {}
    let recording = std::mem::ManuallyDrop::into_inner(recording);
    assert_eq!(
        recording.armed(),
        Some((Some("Zen".to_owned()), 90.0)),
        "a fresh grace"
    );
    recording.sleepers(0);
    assert!(resumed.wait_for_sleepers(1), "the fresh countdown sleeps");
    resumed.advance(Duration::from_secs(89));
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    assert!(resumed.wait_for_sleepers(1));
    resumed.advance(Duration::from_secs(1));
    recording
        .idle("the fresh grace stopped the recording")
        .await;
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
}

/// A recording whose audio service restarts a second into it (coreaudiod
/// restarting, say): the restarts fail past the attempts, so the
/// capture warns that it is still trying, and then one runs.
fn recording_whose_audio_service_restarts() -> Recording {
    recording_over(synthetic_recorder_over(Arc::new(|configuration| {
        let options = crate::testing::synthetic_tone(&configuration)
            .change_device_after(1.0)
            .change_reason(steno_audio::DeviceChangeReason::AudioServiceRestarted)
            .restarts_that_fail(steno_audio::CaptureSession::RESTART_ATTEMPTS);
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })))
}

/// What the status shows at one of the capture's notices.
#[derive(Debug)]
struct AtNotice {
    armed: Option<f64>,
    warning: Option<String>,
}

/// Holds the capture's notice thread in the change hook after each of its
/// first `notices` notices, so the recorder's view of the capture stays
/// there until the test lets it go on: each one sends the status and
/// waits for the reply.
fn hold_the_notices(
    recording: &Recording,
    notices: usize,
) -> (
    std::sync::mpsc::Receiver<AtNotice>,
    std::sync::mpsc::Sender<()>,
) {
    let (at, seen) = std::sync::mpsc::channel();
    let (go, gone) = std::sync::mpsc::channel::<()>();
    let gone = std::sync::Mutex::new(gone);
    let held = std::sync::atomic::AtomicUsize::new(0);
    let watched = Arc::downgrade(&recording.recorder);
    recording.recorder.on_change(Arc::new(move || {
        let Some(recorder) = watched.upgrade() else {
            return;
        };
        if !on_the_notice_thread() || held.fetch_add(1, Ordering::SeqCst) >= notices {
            return;
        }
        let status = recorder.status();
        drop(recorder);
        let _ = at.send(AtNotice {
            armed: status.auto_stop.map(|armed| armed.remaining_seconds),
            warning: status.warning,
        });
        let _ = gone.lock().unwrap().recv_timeout(PATIENCE);
    }));
    (seen, go)
}

/// Asserts the recording goes on for a moment: a countdown that ran out
/// while the capture recovers ends nothing, however soon after it woke.
fn assert_still_recording(recording: &Recording) {
    let until = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < until {
        assert_eq!(recording.recorder.status().state, RecordingState::Recording);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A restart of the audio service empties the call detector's process
/// list, so the call app seems to let go of the microphone, before the
/// capture's change (`released_before`) or during its recovery: a
/// recovery shorter than the grace ends no recording and shows no
/// countdown, the warning dismissed and the stream resumed included, and
/// once audio is back with the app still holding no microphone a fresh
/// 90 s countdown runs and then ends the call.
async fn a_restarted_audio_service_ends_no_call(released_before: bool) {
    let recording = recording_whose_audio_service_restarts();
    let (seen, go) = hold_the_notices(&recording, 3);
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    if released_before {
        recording.released();
        recording.sleepers(1);
    }
    let changed = seen.recv_timeout(PATIENCE).expect("the service restarted");
    assert_eq!(changed.armed, None, "no countdown shown while it recovers");
    if !released_before {
        recording.released();
        recording.sleepers(1);
        assert_eq!(
            recording.armed(),
            None,
            "no countdown shown while the capture recovers"
        );
    }
    go.send(()).unwrap();
    let still_restarting = seen.recv_timeout(PATIENCE).expect("the restarts went on");
    assert_eq!(
        still_restarting.warning.as_deref(),
        Some("No audio is arriving. Still trying.")
    );
    recording.recorder.clear_messages();
    assert_eq!(recording.recorder.status().warning, None, "dismissed");
    go.send(()).unwrap();
    seen.recv_timeout(PATIENCE).expect("the capture resumed");
    // Resumed after the restarts went on, so not yet back: still no
    // countdown shown, and the recording goes on.
    recording.clock.advance(Duration::from_secs(60));
    recording.sleepers(1);
    assert_still_recording(&recording);
    assert_eq!(recording.armed(), None, "still none shown");
    // The fresh countdown on a clock of its own, at the same time, so
    // each clock's sleepers are its own countdown's.
    let resumed = Arc::new(ManualClock::new());
    resumed.advance(Duration::from_secs(60));
    recording.recorder.count_down_on(resumed.clone());
    go.send(()).unwrap();
    // Audio is back (`Delivering`): a fresh grace from now.
    eventually("a fresh countdown once audio is back", || {
        recording.armed() == Some((Some("Zen".to_owned()), 90.0))
    })
    .await;
    recording.sleepers(0);
    assert!(resumed.wait_for_sleepers(1), "the fresh countdown sleeps");
    resumed.advance(Duration::from_secs(89));
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    assert!(resumed.wait_for_sleepers(1));
    resumed.advance(Duration::from_secs(1));
    eventually("the fresh grace stopped the recording", || {
        recording.recorder.status().state == RecordingState::Idle
    })
    .await;
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_before_the_audio_service_restarted_ends_no_call() {
    a_restarted_audio_service_ends_no_call(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_while_the_audio_service_restarts_ends_no_call() {
    a_restarted_audio_service_ends_no_call(false).await;
}

/// Asserts the one meeting was ended by the call app's release and saved
/// as Stop saves it: handed to processing, its asset on disk.
fn assert_saved_as_the_call_ended(recording: &Recording) {
    let meetings = recording.meetings();
    let meeting = &meetings[0];
    assert_eq!(
        meeting.end_reason,
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
    assert!(
        !matches!(
            meeting.state,
            MeetingState::Recording | MeetingState::Failed { .. }
        ),
        "handed to processing, got {:?}",
        meeting.state
    );
    let asset = recording
        .store
        .asset(meeting.id)
        .unwrap()
        .expect("the asset row");
    assert!(
        steno_core::paths::file_url_path(&asset.url).is_some_and(|path| path.exists()),
        "the recording is kept on disk"
    );
}

/// A recording whose capture stalls a second into it and whose restarts
/// all fail until the stop: a Mac call capture whose silent output keeps
/// failing once the hang-up stopped the call app's output, which
/// delivers nothing until something plays. `StillRestarting`, and no
/// `Delivering`.
fn recording_whose_capture_keeps_failing() -> Recording {
    recording_over(synthetic_recorder_over(Arc::new(|configuration| {
        let options = crate::testing::synthetic_tone(&configuration)
            .change_device_after(1.0)
            .change_reason(steno_audio::DeviceChangeReason::DeliveryStalled)
            .restarts_that_fail(usize::MAX);
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })))
}

/// The cap on the wait for a recovering capture: a call whose capture
/// keeps failing after the call app let go is not kept recording for
/// good. The release's countdown, not shown while the capture recovers,
/// runs out into a fresh 90 s shown at once (never "Stops in 0:00"), and
/// that one ends the call, saved, though the capture still recovers: two
/// graces after the release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_that_keeps_failing_ends_the_call_two_graces_after_the_release() {
    let recording = recording_whose_capture_keeps_failing();
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    let still_trying = || {
        recording
            .recorder
            .status()
            .warning
            .is_some_and(|warning| warning.ends_with("Still trying."))
    };
    eventually("the restarts went on", still_trying).await;
    assert_eq!(recording.armed(), None, "not shown while it recovers");
    recording.clock.advance(AUTO_STOP_GRACE);
    let fresh = Some((Some("Zen".to_owned()), 90.0));
    eventually("a fresh grace in its place", || {
        let armed = recording.armed();
        assert!(
            armed.as_ref().is_none_or(|(_, seconds)| *seconds > 0.0),
            "{armed:?}"
        );
        armed == fresh
    })
    .await;
    assert_still_recording(&recording);
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(89));
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    assert!(still_trying(), "the capture still recovers");
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(1));
    eventually("the fresh grace stopped the recording", || {
        recording.recorder.status().state == RecordingState::Idle
    })
    .await;
    assert_saved_as_the_call_ended(&recording);
}

/// The cap on the wait, for a capture that recovers over and over without
/// `StillRestarting` (a stream that stalls again within 10 s of each
/// resume, or devices that change a minute apart): only the first resume
/// puts a fresh 90 s in the place of the release's countdown, the later
/// ones leave it, and it ends the call, saved, within two graces of the
/// release (here at 150 s, the first recovery a minute in).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recoveries_over_and_over_end_the_call_within_two_graces_of_the_release() {
    let backend: Arc<std::sync::Mutex<Option<Arc<steno_audio::testing::SyntheticCaptureBackend>>>> =
        Arc::default();
    let made = backend.clone();
    let recording = recording_over(synthetic_recorder_over(Arc::new(move |configuration| {
        let options = crate::testing::synthetic_tone(&configuration)
            .change_device_after(1.0)
            .changes(4);
        let synthetic = Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options));
        *made.lock().unwrap() = Some(synthetic.clone());
        steno_audio::CaptureSession::with_backend(
            configuration,
            synthetic,
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })));
    let starts = || {
        backend
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |synthetic| synthetic.starts())
    };
    let (seen, go) = hold_the_notices(&recording, 1);
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    let changed = seen.recv_timeout(PATIENCE).expect("the device changed");
    assert_eq!(changed.armed, None, "not shown while it recovers");
    // The first recovery is a minute in on the countdown's clock. The
    // fresh countdown on a clock of its own, at the same time, so each
    // clock's sleepers are its own countdown's.
    recording.clock.advance(Duration::from_secs(60));
    let resumed = Arc::new(ManualClock::new());
    resumed.advance(Duration::from_secs(60));
    recording.recorder.count_down_on(resumed.clone());
    go.send(()).unwrap();
    eventually("the resume put a fresh grace in its place", || {
        recording.armed() == Some((Some("Zen".to_owned()), 90.0))
    })
    .await;
    recording.sleepers(0);
    assert!(resumed.wait_for_sleepers(1), "the fresh countdown sleeps");
    resumed.advance(Duration::from_secs(60));
    // The three later recoveries, a second apart on the wall clock.
    eventually_within(
        Duration::from_secs(20),
        "the device changed four times",
        || starts() == 5,
    )
    .await;
    assert_still_recording(&recording);
    assert_eq!(
        recording.armed(),
        Some((Some("Zen".to_owned()), 30.0)),
        "no later resume replaced it"
    );
    assert!(resumed.wait_for_sleepers(1));
    resumed.advance(Duration::from_secs(29));
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    assert!(resumed.wait_for_sleepers(1));
    resumed.advance(Duration::from_secs(1));
    eventually("the fresh grace stopped the recording", || {
        recording.recorder.status().state == RecordingState::Idle
    })
    .await;
    assert_saved_as_the_call_ended(&recording);
}

/// Where a test holds a rebuild's start: the backend says when the start
/// is waiting, and goes on once the test opens it.
struct Gate {
    entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    open: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

/// The synthetic backend whose second restart waits at `gate`, so that
/// rebuild's start holds the session's lock until the test opens it.
struct GatedRestart {
    synthetic: steno_audio::testing::SyntheticCaptureBackend,
    starts: std::sync::atomic::AtomicUsize,
    gate: Arc<Gate>,
}

impl steno_audio::CaptureBackend for GatedRestart {
    fn start(
        &self,
        lanes: &[steno_core::AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<steno_audio::LaneFrameSink>,
    ) -> Result<steno_audio::CaptureStream, steno_audio::CaptureError> {
        if self.starts.fetch_add(1, Ordering::SeqCst) == 2 {
            let _ = self.gate.entered.lock().unwrap().send(());
            let _ = self.gate.open.lock().unwrap().recv_timeout(PATIENCE);
        }
        self.synthetic.start(lanes, input_device_uid, sink)
    }

    fn stop(&self) {
        self.synthetic.stop();
    }
}

/// A recording whose device changes twice for `reason`, a second into
/// each start, and whose second rebuild's start waits at a gate: the
/// receiver hears when it waits, and the sender opens it.
fn recording_whose_second_rebuild_waits(
    reason: steno_audio::DeviceChangeReason,
) -> (
    Recording,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, waiting) = std::sync::mpsc::channel();
    let (open, opened) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        entered: std::sync::Mutex::new(entered),
        open: std::sync::Mutex::new(opened),
    });
    let recording = recording_over(synthetic_recorder_over(Arc::new(move |configuration| {
        let options = crate::testing::synthetic_tone(&configuration)
            .change_device_after(1.0)
            .changes(2)
            .change_reason(reason);
        let backend = GatedRestart {
            synthetic: steno_audio::testing::SyntheticCaptureBackend::new(options),
            starts: std::sync::atomic::AtomicUsize::new(0),
            gate: gate.clone(),
        };
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(backend),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })));
    (recording, waiting, open)
}

/// The notice thread reads the session with the outage's lock released:
/// a rebuild's start holds the session's lock for seconds, and the
/// status, the microphone's activity and the countdown's end, which take
/// the outage's lock, do not wait for it. Here the thread handles a
/// resume while the next rebuild's start waits at a gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_s_start_holds_up_neither_the_status_nor_the_auto_stop() {
    let (recording, waiting, open) =
        recording_whose_second_rebuild_waits(steno_audio::DeviceChangeReason::DefaultInputChanged);
    // The first change's notice waits until the second rebuild's start
    // holds the session, so the thread reads the session for the first
    // resume while that start runs.
    let (go, gone) = std::sync::mpsc::channel::<()>();
    let gone = std::sync::Mutex::new(gone);
    let once = AtomicBool::new(false);
    recording.recorder.on_change(Arc::new(move || {
        if on_the_notice_thread() && !once.swap(true, Ordering::SeqCst) {
            let _ = gone.lock().unwrap().recv_timeout(PATIENCE);
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    waiting
        .recv_timeout(PATIENCE)
        .expect("the second rebuild's start waits");
    go.send(()).unwrap();
    let recorder = recording.recorder.clone();
    on_own_thread(
        PATIENCE,
        "the status returned while a start ran",
        move || {
            let until = std::time::Instant::now() + Duration::from_millis(300);
            while std::time::Instant::now() < until {
                assert_eq!(recorder.status().state, RecordingState::Recording);
                std::thread::sleep(Duration::from_millis(5));
            }
        },
    );
    open.send(()).unwrap();
    recording.stop();
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::Manual)
    );
}

/// A device change begins the recovery before the notice thread reads the
/// session for the stalled microphone, a read that waits behind the
/// rebuild's start (a call on Windows runs on the microphone's clock, and
/// a start there takes up to 10 s): a release's countdown that runs out
/// meanwhile is not shown, ends nothing, and a fresh one takes its place
/// (S2 in `.plans/2026-10-07-stable-promotion.md`). Elsewhere a call's
/// change reads nothing, and the test pins the same outcome. Here the
/// second stall's rebuild waits at a gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_countdown_that_runs_out_while_a_stall_s_rebuild_starts_ends_no_call() {
    let (recording, waiting, open) =
        recording_whose_second_rebuild_waits(steno_audio::DeviceChangeReason::DeliveryStalled);
    let (seen, go) = hold_the_notices(&recording, 2);
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    seen.recv_timeout(PATIENCE).expect("the first stall");
    go.send(()).unwrap();
    seen.recv_timeout(PATIENCE).expect("the first resume");
    // Back from the first stall: the release's countdown runs, shown.
    recording.released();
    recording.sleepers(1);
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 90.0)));
    waiting
        .recv_timeout(PATIENCE)
        .expect("the second stall's rebuild starts");
    go.send(()).unwrap();
    // Well inside the gate's own patience, so a read that came first
    // fails here rather than once the gate gave up.
    eventually_within(PATIENCE / 2, "not shown from the second stall on", || {
        recording.armed().is_none()
    })
    .await;
    recording.clock.advance(Duration::from_secs(90));
    eventually("a fresh grace in its place", || {
        recording.armed() == Some((Some("Zen".to_owned()), 90.0))
    })
    .await;
    assert_still_recording(&recording);
    open.send(()).unwrap();
    recording.stop();
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::Manual)
    );
}

/// Audio back after the capture recovered arms a fresh countdown when the
/// call app let go and holds no microphone, since the detector does not
/// report that release again; the earlier countdown is withdrawn, and the
/// fresh one ends the call. The change hook reads the status, as the
/// app's does, so a resume that notified under the recorder's lock would
/// deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_back_arms_a_fresh_countdown_once_the_call_app_let_go() {
    // Not dropped until the resumes returned: a deadlocked resume holds
    // the recorder's lock, and the drop's stop would wait on it.
    let recording = std::mem::ManuallyDrop::new(recording());
    let watched = Arc::downgrade(&recording.recorder);
    recording.recorder.on_change(Arc::new(move || {
        if let Some(recorder) = watched.upgrade() {
            let _ = recorder.status();
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.resume();
    assert_eq!(
        recording.armed(),
        None,
        "the app still holds the microphone"
    );
    recording.released();
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(30));
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 60.0)));
    // The fresh countdown on a clock of its own, at the same time, so
    // each clock's sleepers are its own countdown's.
    let resumed = Arc::new(ManualClock::new());
    resumed.advance(Duration::from_secs(30));
    recording.recorder.count_down_on(resumed.clone());
    recording.resume();
    let recording = std::mem::ManuallyDrop::into_inner(recording);
    assert_eq!(
        recording.armed(),
        Some((Some("Zen".to_owned()), 90.0)),
        "a fresh grace"
    );
    recording.sleepers(0);
    assert!(resumed.wait_for_sleepers(1), "the fresh countdown sleeps");
    resumed.advance(Duration::from_secs(89));
    assert_eq!(recording.recorder.status().state, RecordingState::Recording);
    assert!(resumed.wait_for_sleepers(1));
    resumed.advance(Duration::from_secs(1));
    recording
        .idle("the fresh grace stopped the recording")
        .await;
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned())
        })
    );
}

/// The capture's notice thread may resume the auto-stop while a stop is
/// on its way to joining that thread: the resume takes the recorder's
/// lock only to arm and the stop does not hold it while it joins, so
/// neither waits for the other, and a stopping recording arms nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_on_the_notice_thread_during_a_stop_does_not_deadlock() {
    let recording = recording_whose_device_changes();
    let (entered, in_hook) = std::sync::mpsc::channel();
    let (stopping, stop_began) = std::sync::mpsc::channel();
    let stop_began = std::sync::Mutex::new(stop_began);
    let once = AtomicBool::new(false);
    let resumed = Arc::new(AtomicBool::new(false));
    let watched = Arc::downgrade(&recording.recorder);
    let hook_resumed = resumed.clone();
    recording.recorder.on_change(Arc::new(move || {
        let Some(recorder) = watched.upgrade() else {
            return;
        };
        if on_the_notice_thread() {
            if once.swap(true, Ordering::SeqCst) {
                return;
            }
            let _ = entered.send(());
            stop_began
                .lock()
                .unwrap()
                .recv_timeout(PATIENCE)
                .expect("the stop began");
            recorder.resume_auto_stop();
            hook_resumed.store(true, Ordering::SeqCst);
        } else if recorder.status().state == RecordingState::Stopping {
            let _ = stopping.send(());
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    in_hook.recv_timeout(PATIENCE).expect("the device changed");
    recording.stop();
    assert!(resumed.load(Ordering::SeqCst), "the resume returned");
    assert_eq!(recording.recorder.status().state, RecordingState::Idle);
    assert_eq!(recording.armed(), None, "nothing armed while stopping");
    assert_eq!(
        recording.meetings()[0].end_reason,
        Some(RecordingEndReason::Manual)
    );
}
