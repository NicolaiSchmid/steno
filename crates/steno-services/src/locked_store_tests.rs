//! What stays live while another process holds the database's write lock
//! and a write of this process waits behind it: the checks of
//! `.plans/2026-10-12-store-off-the-workers.md` ("Acceptance") that need
//! the recorder, the host and the store together. Each holds the lock for
//! [`HeldStore::HOLD`] from another connection, with a write of this
//! process waiting out its busy timeout behind it, as a pipeline write
//! does, and a reload in flight, as the two-second poll runs one. The
//! host reads on its own reader, so neither wait reaches it. "Live" is
//! [`LIVE`]. Each runs on a runtime with one worker, so a wait on that
//! worker would show too. Rust only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_bridge::{BridgeHost as _, BridgeTopic, CaptureMode, MeetingIdParams, RecordingState};
use steno_host::services::Recorder as _;

use crate::testing::{
    HANG_GUARD, HeldStore, SyntheticRecorder, hosted, synthetic_recorder, synthetic_recorder_over,
};

/// What "live" means here: well under the busy timeout the waiting write
/// runs out.
const LIVE: Duration = Duration::from_millis(200);

/// Waits until the recording sends its levels to the host.
fn levels_flow(sink: &crate::testing::TimedSink, since: Instant) {
    assert!(
        sink.recording_in("recording", since, HANG_GUARD).is_some(),
        "the recording reached the host"
    );
}

/// P40 check 1, the pipeline write's case: while the write waits, each
/// level update's `recorder_changed` returns live, and a `recording`
/// snapshot reaches the sink within [`LIVE`] of it, for the whole hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn levels_keep_flowing_while_another_process_holds_the_write_lock() {
    let capture = synthetic_recorder();
    let hosted = hosted(&capture.store, &capture.recorder);
    capture.start(CaptureMode::InPerson, None);
    levels_flow(&hosted.sink, Instant::now());

    let mut held = HeldStore::with_a_write_waiting(&capture.store, &capture.database());
    let reload = hosted.reload_in_flight();
    let since = Instant::now();
    let released = held.release_after_hold();

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
        calls.len() >= 10,
        "the levels kept calling the hook: {} calls",
        calls.len()
    );
    for call in &calls {
        assert!(
            call.took < LIVE,
            "a level update's hook took {:?}",
            call.took
        );
        assert!(
            arrivals
                .iter()
                .any(|at| *at >= call.began && *at <= call.began + LIVE),
            "no recording snapshot within {LIVE:?} of a level update {:?} into the hold",
            call.began.duration_since(since)
        );
    }
    reload.join().unwrap();
    capture.stop();
}

/// A recording whose input device changes `after` seconds into it: the
/// capture rebuilds on the new one and its notice thread calls the change
/// hook.
fn recording_whose_device_changes(after: f64) -> SyntheticRecorder {
    synthetic_recorder_over(Arc::new(move |configuration| {
        let options = crate::testing::synthetic_tone(&configuration).change_device_after(after);
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    }))
}

/// P40 check 2, the pipeline write's case: a device change while the
/// write waits reaches the host through the notice thread's hook, which
/// returns live, and the auto-stop's countdown, hidden while the capture
/// recovers, is armed again within [`LIVE`] of the first notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_device_notice_reaches_the_host_while_a_write_waits() {
    let capture = recording_whose_device_changes(2.0);
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

    let mut held = HeldStore::with_a_write_waiting(&capture.store, &capture.database());
    let reload = hosted.reload_in_flight();
    let since = Instant::now();
    let on_the_notices = |calls: &[crate::testing::HookCall]| {
        calls
            .iter()
            .filter(|call| call.thread.as_deref() == Some("steno-notices"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let deadline = since + HANG_GUARD;
    let notices = loop {
        let notices = on_the_notices(&hosted.hooks_since(since));
        if notices.iter().any(|call| call.armed) {
            break notices;
        }
        assert!(Instant::now() < deadline, "the capture resumed");
        std::thread::sleep(Duration::from_millis(5));
    };
    let released = held.release_after_hold();

    let first = &notices[0];
    let armed = notices.iter().find(|call| call.armed).unwrap();
    assert!(
        armed.began + armed.took < released,
        "the countdown was armed while the write still waited"
    );
    assert!(
        !first.armed,
        "the countdown is hidden while the capture recovers"
    );
    let slowest = notices.iter().map(|call| call.took).max().unwrap();
    assert!(slowest < LIVE, "a notice's hook took {slowest:?}");
    let rearmed = (armed.began + armed.took).duration_since(first.began);
    assert!(
        rearmed < LIVE,
        "the countdown was armed again {rearmed:?} after the notice"
    );
    reload.join().unwrap();
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

    let mut held = HeldStore::with_a_write_waiting(&capture.store, &capture.database());
    let reload = hosted.reload_in_flight();
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
    held.release_after_hold();
    reload.join().unwrap();
}
