//! Swift's `AutoStopTests`: the policy as a table over [`CallWatch`], then
//! the recorder over the synthetic capture and a `ManualClock`, its
//! meetings read from the store. No sleeps: the clock is advanced by hand
//! and the tests wait for its sleepers.

use std::sync::Arc;
use std::time::Duration;

use steno_audio::testing::ManualClock;
use steno_bridge::{CaptureMode, RecordingState};
use steno_core::{MeetingState, RecordingEndReason};
use steno_host::services::Recorder as _;

use super::*;
use crate::testing::{
    PATIENCE, SyntheticRecorder, eventually, on_own_thread, synthetic_recorder,
    synthetic_recorder_over,
};

/// One step of a [`CallWatch`] script.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// A recording starts, from the prompt for this app or not.
    Begin(Option<&'static str>),
    Opened(Option<&'static str>),
    Released,
    Keep,
    /// Audio delivered again after the capture recovered.
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
    for step in script {
        match *step {
            Step::Begin(app) => watch.begin(app),
            Step::Opened(app) => watch.opened(app.map(str::to_owned)),
            Step::Released => {
                watch.released(now);
            }
            Step::Keep => {
                watch.keep_recording();
            }
            Step::Resume => {
                watch.resume(now);
            }
            Step::Reset => watch.reset(),
            Step::Wait(seconds) => now += Duration::from_secs_f64(seconds),
        }
    }
    watch
        .status(now)
        .map(|status| (status.app_name, status.remaining_seconds))
}

#[test]
#[allow(clippy::too_many_lines, reason = "one row per case of the table")]
fn the_policy_arms_cancels_and_forgets_as_swift_s_did() {
    use Step::{Begin, Keep, Opened, Released, Reset, Resume, Wait};
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
    assert_eq!(watch.status(Duration::ZERO), None, "and it is spent");

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
        let (recorder, call_app) = (self.recorder.clone(), call_app.map(str::to_owned));
        on_own_thread(PATIENCE, "the start returned", move || {
            recorder.start(mode, call_app.as_deref());
        });
        assert_eq!(self.recorder.status().state, RecordingState::Recording);
    }

    fn stop(&self) {
        let recorder = self.recorder.clone();
        on_own_thread(PATIENCE, "the stop returned", move || recorder.stop());
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
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(1));
    eventually("the grace stopped the recording", || {
        recording.recorder.status().state == RecordingState::Idle
    })
    .await;
    let status = recording.recorder.status();
    assert_eq!(status.auto_stop, None);
    assert_eq!(status.error, None);
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
    eventually("the grace stopped the recording", || {
        recording.recorder.status().state == RecordingState::Idle
    })
    .await;
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
        // What the bubble shows while the stop saves: the countdown is
        // gone from its start, as Swift's stop disarmed first.
        let while_stopping = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (seen, watched) = (while_stopping.clone(), Arc::downgrade(&recording.recorder));
        recording.recorder.on_change(Arc::new(move || {
            if let Some(recorder) = watched.upgrade() {
                let status = recorder.status();
                if status.state == RecordingState::Stopping {
                    seen.lock().unwrap().push(status.auto_stop);
                }
            }
        }));
        if quit {
            let recorder = recording.recorder.clone();
            on_own_thread(PATIENCE, "the quit returned", move || {
                recorder.stop_for_quit();
            });
        } else {
            recording.stop();
        }
        assert_eq!(recording.recorder.status().state, RecordingState::Idle);
        assert_eq!(recording.armed(), None);
        let while_stopping = while_stopping.lock().unwrap().clone();
        assert!(!while_stopping.is_empty(), "the stop was seen");
        assert!(
            while_stopping.iter().all(Option::is_none),
            "no countdown while the stop saves: {while_stopping:?}"
        );
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
    let once = std::sync::Once::new();
    recording.recorder.on_change(Arc::new(move || {
        if let Some(recorder) = watched.upgrade()
            && recorder.status().state == RecordingState::Starting
        {
            // Once: the change it reports comes back here.
            let mut first = false;
            once.call_once(|| first = true);
            if !first {
                return;
            }
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

/// Swift's `testADeviceChangeNoticeLeavesTheCountdownRunning`: a device
/// change during the countdown leaves it as it was, and it still ends the
/// call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_change_leaves_the_countdown_running() {
    let recording = recording_whose_device_changes();
    let (changed, rebuilt) = std::sync::mpsc::channel();
    let watched = Arc::downgrade(&recording.recorder);
    recording.recorder.on_change(Arc::new(move || {
        if on_the_notice_thread()
            && let Some(recorder) = watched.upgrade()
        {
            let _ = changed.send(recorder.status().auto_stop.is_some());
        }
    }));
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.released();
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(10));
    let armed_then = rebuilt.recv_timeout(PATIENCE).expect("the device changed");
    assert!(armed_then, "the countdown ran when the device changed");
    assert_eq!(recording.armed(), Some((Some("Zen".to_owned()), 80.0)));
    recording.sleepers(1);
    recording.clock.advance(Duration::from_secs(80));
    eventually("the grace stopped the recording", || {
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

/// Audio back after the capture recovered arms a fresh countdown when the
/// call app let go and holds no microphone, since the detector does not
/// report that release again; the earlier countdown is withdrawn, and the
/// fresh one ends the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_back_arms_a_fresh_countdown_once_the_call_app_let_go() {
    let recording = recording();
    recording.start(CaptureMode::Call, None);
    recording.opened(Some("Zen"));
    recording.recorder.resume_auto_stop();
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
    recording.recorder.resume_auto_stop();
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

/// The capture's notice thread may resume the auto-stop while a stop is
/// on its way to joining that thread: the resume takes the recorder's
/// lock only to arm and the stop does not hold it while it joins, so
/// neither waits for the other, and a stopping recording arms nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_on_the_notice_thread_during_a_stop_does_not_deadlock() {
    use std::sync::atomic::{AtomicBool, Ordering};
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
