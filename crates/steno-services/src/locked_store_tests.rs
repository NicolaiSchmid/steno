//! What stays live while another process holds the database's write lock
//! and a write of this process waits behind it: the checks of
//! `.plans/2026-10-12-store-off-the-workers.md` ("Acceptance") that need
//! the recorder, the host and the store together. Each holds the lock for
//! [`HeldStore::HOLD`] from another connection, with a write of this
//! process waiting out its busy timeout behind it ([`Waiting`]): a
//! pipeline write, with a forced reload in flight as the reload owner runs
//! one, which the host's own reader keeps from waiting; or, for checks 1
//! and 2, a settings save, which waits with the view-model lock held. The
//! `recording` feed and the recorder's hook take no host lock, so neither
//! wait reaches them. "Live" is [`LIVE`]. Each runs on a runtime with one
//! worker, so a wait on that worker would show too. Rust only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use steno_bridge::{
    BridgeHost as _, BridgeTopic, CaptureMode, MeetingIdParams, RecordingState, SetBoolParams,
};
use steno_host::services::Recorder as _;

use crate::testing::{
    END_TO_END, HANG_GUARD, HeldOutages, HeldStore, Hosted, LIVE, SyntheticRecorder, found_within,
    hosted, synthetic_recorder, synthetic_recorder_over,
};

/// What waits behind the held lock in a check.
#[derive(Debug, Clone, Copy)]
enum Waiting {
    /// A write of this process, as a pipeline write does, and a forced
    /// reload in flight.
    PipelineWrite,
    /// A settings save, tried again whenever its busy timeout runs out,
    /// which holds the host's view-model lock while it waits: the host's
    /// writes keep that lock until the host actor.
    SettingsSave,
}

/// A held lock and what waits behind it ([`Waiting::begin`]).
struct Wait {
    held: HeldStore,
    behind: Option<std::thread::JoinHandle<()>>,
    released: Arc<AtomicBool>,
}

impl Waiting {
    /// Holds the lock on `capture`'s database and starts what waits behind
    /// it; returns once that is in its wait.
    fn begin(self, capture: &SyntheticRecorder, hosted: &Hosted) -> Wait {
        let released = Arc::new(AtomicBool::new(false));
        match self {
            Waiting::PipelineWrite => Wait {
                held: HeldStore::with_a_write_waiting(&capture.store, &capture.database()),
                behind: Some(hosted.reload_in_flight()),
                released,
            },
            Waiting::SettingsSave => {
                let held = HeldStore::lock_only(&capture.database());
                let (host, stop) = (hosted.host.clone(), released.clone());
                let behind = std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        let _ = host.settings_general_set_detection(SetBoolParams { value: false });
                    }
                });
                // The save is in its busy wait by now; nothing reports it.
                std::thread::sleep(std::time::Duration::from_millis(100));
                Wait {
                    held,
                    behind: Some(behind),
                    released,
                }
            }
        }
    }
}

impl Wait {
    /// Lets the lock go once [`HeldStore::HOLD`] has passed, and waits for
    /// what waited; returns when the lock went.
    fn release_after_hold(&mut self) -> Instant {
        self.released.store(true, Ordering::SeqCst);
        let released = self.held.release_after_hold();
        if let Some(behind) = self.behind.take() {
            behind.join().unwrap();
        }
        released
    }
}

/// Waits until the recording sends its levels to the host.
fn levels_flow(sink: &crate::testing::TimedSink, since: Instant) {
    assert!(
        sink.recording_in("recording", since, HANG_GUARD).is_some(),
        "the recording reached the host"
    );
}

/// P40 check 1: while a pipeline write waits, and while a settings save
/// waits with the view-model lock held, each level update's
/// `recorder_changed` returns live, and a `recording` snapshot reaches the
/// sink within [`LIVE`] of it, for the whole hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn levels_keep_flowing_while_another_process_holds_the_write_lock() {
    let capture = synthetic_recorder();
    let hosted = hosted(&capture.store, &capture.recorder);
    capture.start(CaptureMode::InPerson, None);
    levels_flow(&hosted.sink, Instant::now());
    for waiting in [Waiting::PipelineWrite, Waiting::SettingsSave] {
        levels_flow_while(waiting, &capture, &hosted);
    }
    capture.stop();
}

/// Check 1's assertions for one [`Waiting`].
fn levels_flow_while(waiting: Waiting, capture: &SyntheticRecorder, hosted: &Hosted) {
    let mut wait = waiting.begin(capture, hosted);
    let since = Instant::now();
    let released = wait.release_after_hold();

    let arrivals: Vec<Instant> = hosted
        .sink
        .since(BridgeTopic::Recording, since)
        .into_iter()
        .map(|(at, _)| at)
        .collect();
    let calls: Vec<_> = hosted
        .hooks_since(since)
        .into_iter()
        .filter(|call| call.began + LIVE < released)
        .collect();
    assert!(
        calls.len() >= 3,
        "{waiting:?}: the levels kept calling the hook: {} calls",
        calls.len()
    );
    for call in &calls {
        assert!(
            call.took < LIVE,
            "{waiting:?}: a level update's hook took {:?}",
            call.took
        );
        assert!(
            arrivals
                .iter()
                .any(|at| *at >= call.began && *at <= call.began + LIVE),
            "{waiting:?}: no recording snapshot within {LIVE:?} of a level update {:?} into the hold",
            call.began.duration_since(since)
        );
    }
}

/// A recording whose input device changes `after` seconds into it, once
/// `outages` lets the change go: the capture rebuilds on the new one and
/// its notice thread calls the change hook.
fn recording_whose_device_changes(after: f64, outages: &HeldOutages) -> SyntheticRecorder {
    let outages = outages.clone();
    synthetic_recorder_over(Arc::new(move |configuration| {
        let options = crate::testing::synthetic_tone(&configuration).change_device_after(after);
        steno_audio::CaptureSession::with_backend(
            configuration,
            outages.backend(options),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    }))
}

/// P40 check 2: a device change while a pipeline write waits, and while a
/// settings save waits with the view-model lock held, reaches the host
/// through the notice thread's hook, which returns live each time, so the
/// auto-stop's countdown, hidden while the capture recovers, is armed
/// again at the capture's resume while the write still waits, within
/// [`END_TO_END`] of the change's first notice: no notice waits for the
/// host. The change is held until the countdown is armed and the write
/// waits. A recording of its own per case, each with one device change.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_device_notice_reaches_the_host_while_a_write_waits() {
    for waiting in [Waiting::PipelineWrite, Waiting::SettingsSave] {
        a_device_notice_reaches_the_host_while(waiting);
    }
}

/// Check 2 for one [`Waiting`].
fn a_device_notice_reaches_the_host_while(waiting: Waiting) {
    let outages = HeldOutages::default();
    let capture = recording_whose_device_changes(2.0, &outages);
    let hosted = hosted(&capture.store, &capture.recorder);
    capture.start(CaptureMode::Call, None);
    capture
        .recorder
        .microphone_activity(crate::auto_stop::MicrophoneActivity::Opened {
            app_name: Some("Zen".to_owned()),
        });
    capture
        .recorder
        .microphone_activity(crate::auto_stop::MicrophoneActivity::Released);
    assert!(
        capture.recorder.status().auto_stop.is_some(),
        "the countdown is armed"
    );

    let mut wait = waiting.begin(&capture, &hosted);
    let since = Instant::now();
    outages.release();
    let on_the_notices = |calls: &[crate::testing::HookCall]| {
        calls
            .iter()
            .filter(|call| call.thread.as_deref() == Some("steno-notices"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let notices = found_within(HANG_GUARD, "the capture resumed", || {
        let notices = on_the_notices(&hosted.hooks_since(since));
        notices.iter().any(|call| call.armed).then_some(notices)
    });
    let released = wait.release_after_hold();

    let first = &notices[0];
    let armed = notices.iter().find(|call| call.armed).unwrap();
    assert!(
        armed.began + armed.took < released,
        "{waiting:?}: the countdown was armed while the write still waited"
    );
    assert!(
        !first.armed,
        "{waiting:?}: the countdown is hidden while the capture recovers"
    );
    let rearmed = (armed.began + armed.took).duration_since(first.began);
    assert!(
        rearmed < END_TO_END,
        "{waiting:?}: the countdown was armed again {rearmed:?} after the change's first notice"
    );
    let slowest = notices.iter().map(|call| call.took).max().unwrap();
    assert!(
        slowest < LIVE,
        "{waiting:?}: a notice's hook took {slowest:?}"
    );
    capture.stop();
}

/// P40 check 6, narrowed to the pipeline write's case: while the write
/// waits, `meetings.select` and a Settings command, which reads the stored
/// settings, answer live, and the detail they select reaches the sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_command_answers_while_a_write_waits() {
    let capture = synthetic_recorder();
    // Two meetings: the first fill selects the newer, so selecting the
    // older one moves the detail.
    let newer = steno_core::testing::sample_data::meeting();
    let mut meeting = newer.clone();
    meeting.id = uuid::Uuid::new_v4();
    meeting.started_at = newer.started_at - chrono::Duration::days(1);
    capture.store.save_meeting(&newer).unwrap();
    capture.store.save_meeting(&meeting).unwrap();
    let hosted = hosted(&capture.store, &capture.recorder);

    let mut wait = Waiting::PipelineWrite.begin(&capture, &hosted);
    let since = Instant::now();
    hosted
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: meeting.id,
        })
        .unwrap();
    let selected = since.elapsed();
    let settings = Instant::now();
    hosted.host.settings_recording_refresh_devices().unwrap();
    let read = settings.elapsed();

    assert!(selected < LIVE, "meetings.select took {selected:?}");
    assert!(read < LIVE, "the Settings command took {read:?}");
    let shown = meeting.id.to_string();
    assert!(
        hosted
            .sink
            .since(BridgeTopic::MeetingDetail, since)
            .iter()
            .any(|(_, detail)| detail["id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(&shown))),
        "the selected meeting's detail reached the sink"
    );
    assert_eq!(capture.recorder.status().state, RecordingState::Idle);
    wait.release_after_hold();
}
