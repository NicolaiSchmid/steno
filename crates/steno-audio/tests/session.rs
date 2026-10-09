//! The session over the synthetic backend: state machine, level stream,
//! files, drop accounting, device changes and loss. Everything runs as fast
//! as the rings accept and the rebuild's backoff runs on a `ManualClock`;
//! no wall-clock sleeps in the code under test.
//! Swift: `Tests/StenoAudioTests/CaptureSessionTests.swift`,
//! `LiveAECPathTests.swift`.

// Test arithmetic: sample counts and dB values cast freely, and sample
// rates compare exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_lossless,
    clippy::unnecessary_wraps
)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_audio::capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureInput, CaptureMode, CaptureNotice,
    CaptureResult, CaptureSession, CaptureState, CaptureStream, DeviceChangeReason, LaneLevel,
};
use steno_audio::realtime::{FrameRelay, LaneFrameSink};
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::testing::{ManualClock, SyntheticCaptureBackend, SyntheticLane};
use steno_audio::writer::{
    CafFile, LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting, WavFile,
};
use steno_audio::{
    Clock, EchoMetrics, FRAME_SIZE, FRAMES_PER_SECOND, PassthroughEchoCanceller, Playback,
    PlaybackRefused, SAMPLE_RATE, SystemClock,
};
use steno_core::paths::file_url_path;
use steno_core::{AudioFormat, AudioLane, AudioRetention, EchoCanceller, RecordingLayout};
use uuid::Uuid;

use common::{frequency, level_against_sine};

const RECV: Duration = Duration::from_secs(10);

fn configuration(mode: CaptureMode, directory: &Path, keep_raw: bool) -> CaptureConfiguration {
    let mut configuration = CaptureConfiguration::new(mode, directory);
    configuration.echo_cancellation = true;
    configuration.keep_raw_mic_lane = keep_raw;
    configuration
}

fn passthrough() -> Option<Box<dyn EchoCanceller>> {
    Some(Box::new(PassthroughEchoCanceller::new(48_000.0, 480)))
}

fn tones(lanes: &[AudioLane], seconds: f64) -> SyntheticOptions {
    SyntheticOptions::tones(
        lanes,
        &[
            (AudioLane::Mic, 440.0),
            (AudioLane::System, 1_000.0),
            (AudioLane::Mixed, 440.0),
        ],
        seconds,
    )
}

fn call() -> [AudioLane; 2] {
    [AudioLane::Mic, AudioLane::System]
}

fn restarted_stream() -> CaptureStream {
    CaptureStream {
        sample_rate: SAMPLE_RATE,
        input_latency_frames: 480,
        output_latency_frames: 9_600,
        layout: None,
        input: None,
    }
}

fn master_of(result: &steno_audio::capture::CaptureResult) -> CafFile {
    CafFile::read(&file_url_path(&result.asset.url).unwrap()).unwrap()
}

fn sidecar_of(result: &steno_audio::capture::CaptureResult, lane: AudioLane) -> Vec<f32> {
    WavFile::read_16k_mono(&file_url_path(&result.asset.sidecars_16k[&lane]).unwrap()).unwrap()
}

/// Collects states until the predicate matches.
fn collect_states(
    states: &Receiver<CaptureState>,
    mut done: impl FnMut(&CaptureState) -> bool,
) -> Vec<CaptureState> {
    let mut seen = Vec::new();
    while let Ok(state) = states.recv_timeout(RECV) {
        let finished = done(&state);
        seen.push(state);
        if finished {
            break;
        }
    }
    seen
}

fn kinds(states: &[CaptureState]) -> Vec<&'static str> {
    states.iter().map(CaptureState::kind).collect()
}

fn until_failed(state: &CaptureState) -> bool {
    matches!(state, CaptureState::Failed { .. })
}

/// Matches the second `Idle`: the one after the initial state a `states()`
/// subscription starts with.
fn until_idle_again() -> impl FnMut(&CaptureState) -> bool {
    let mut idles = 0;
    move |state| {
        if *state == CaptureState::Idle {
            idles += 1;
        }
        idles == 2
    }
}

/// Advances `clock` through the first `count` backoff sleeps of a rebuild,
/// each once the sleeper is registered.
fn advance_through_sleeps(clock: &ManualClock, count: usize) {
    for step in CaptureSession::RESTART_BACKOFF.iter().take(count) {
        assert!(
            clock.wait_for_sleepers(1),
            "the rebuild sleeps on the injected clock"
        );
        clock.advance(*step);
    }
}

/// Gives every thread a chance to run before asserting nothing further
/// happened.
fn settle() {
    std::thread::sleep(Duration::from_millis(100));
}

#[test]
fn idle_starting_recording_stopping_idle_over_the_synthetic_backend() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 3.0)));
    // Ten seconds of writer headroom: the backend delivers three seconds of
    // audio in milliseconds, far faster than a debug-build writer.
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let states = session.states();
    let levels = session.levels();
    let meeting_id = Uuid::new_v4();
    assert_eq!(session.state(), CaptureState::Idle);

    session.start(meeting_id).unwrap();
    assert!(matches!(session.state(), CaptureState::Recording { .. }));

    // The level stream carries the injected tone level (amplitude 0.5 sines
    // are -9.03 dBFS).
    let first = levels.recv_timeout(RECV).unwrap();
    assert!((first.mic.rms - -9.03).abs() < 0.2);
    assert!((first.system.unwrap().rms - -9.03).abs() < 0.2);

    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);

    let seen = collect_states(&states, until_idle_again());
    assert_eq!(
        kinds(&seen),
        ["idle", "starting", "recording", "stopping", "idle"]
    );

    assert_eq!(result.asset.meeting_id, meeting_id);
    assert_eq!(result.asset.format, AudioFormat::Caf48kFloat32);
    assert_eq!(result.asset.lanes, call());
    assert_eq!(result.asset.retention, AudioRetention::KeepForever);
    let layout = RecordingLayout::new(directory.path(), meeting_id);
    assert_eq!(
        file_url_path(&result.asset.url).unwrap(),
        layout.master(AudioFormat::Caf48kFloat32)
    );
    assert_eq!(
        file_url_path(&result.asset.sidecars_16k[&AudioLane::Mic]).unwrap(),
        layout.sidecar(AudioLane::Mic)
    );
    assert_eq!(RecordingLayout::from_asset(&result.asset).unwrap(), layout);
    assert!(result.statistics.dropped_frames.is_empty());
    assert!((result.statistics.duration - 3.0).abs() < 0.02);
    assert!(!result.statistics.system_lane_silent);
    assert!(!result.statistics.ended_on_device_loss);

    let master = master_of(&result);
    assert_eq!(master.channels.len(), 2);
    assert!((master.duration() - 3.0).abs() < 0.02);
    let mic = sidecar_of(&result, AudioLane::Mic);
    assert!((mic.len() as f64 / 16_000.0 - 3.0).abs() < 0.02);
}

/// A device that changes and never comes back: four restarts fail across
/// the backoff ladder and the recording ends in `DeviceLost`, finalised and
/// readable to its last frame, with its [`Playback`] hold released.
#[test]
fn device_lost_stops_cleanly_with_a_readable_master() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(1.0)
            .restarts_that_fail(CaptureSession::RESTART_ATTEMPTS),
    ));
    let playback = Playback::new();
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        clock.clone(),
    )
    .unwrap()
    .with_playback(playback.clone());
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(playback.is_recording());
    advance_through_sleeps(&clock, CaptureSession::RESTART_ATTEMPTS - 1);
    let seen = collect_states(&states, until_failed);
    assert_eq!(
        seen.last().unwrap().failure(),
        Some(&CaptureError::DeviceLost)
    );
    assert!(seen.contains(&CaptureState::Stopping));
    assert!(
        !playback.is_recording(),
        "the device loss released the hold before the state said Failed"
    );

    // The state carries the finalised partial recording; `stop()` returns
    // the same one, with what ended it.
    let result = session.stop().unwrap();
    assert_eq!(result.failure, Some(CaptureError::DeviceLost));
    assert_eq!(
        *seen.last().unwrap(),
        CaptureState::Failed {
            error: CaptureError::DeviceLost,
            recording: Some(Box::new(result.clone()))
        }
    );
    assert!(result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 0);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert_eq!(result.statistics.duration, 1.0);
    let master = master_of(&result);
    assert_eq!(master.channels.len(), 2);
    assert_eq!(master.frame_count(), 48_000);
    assert_eq!(backend.starts(), 1 + CaptureSession::RESTART_ATTEMPTS);

    // A failed session starts again; this backend's one change is spent, so
    // the second recording sees no change and no rebuild.
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let second = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert_eq!(
        backend.starts(),
        2 + CaptureSession::RESTART_ATTEMPTS,
        "one start, no restart"
    );
    assert_eq!(second.statistics.device_changes, 0);
}

/// A rebuild that panics (here the restarted backend's `start`) ends the
/// recording as a device that stayed lost does: `Failed(DeviceLost)` with
/// the recording up to the change, rather than `Recording` over a session
/// that writes nothing any more.
#[test]
fn a_rebuild_that_panics_ends_in_device_lost_with_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(1.0)
            .restart_panics(),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        Arc::new(ManualClock::new()),
    )
    .unwrap();
    let states = session.states();
    session.start(Uuid::new_v4()).unwrap();
    let seen = collect_states(&states, until_failed);
    assert!(seen.contains(&CaptureState::Stopping));
    let result = session.stop().unwrap();
    assert_eq!(
        *seen.last().unwrap(),
        CaptureState::Failed {
            error: CaptureError::DeviceLost,
            recording: Some(Box::new(result.clone()))
        }
    );
    assert_eq!(result.failure, Some(CaptureError::DeviceLost));
    assert!(result.statistics.ended_on_device_loss);
    assert_eq!(master_of(&result).frame_count(), 48_000);
    assert_eq!(
        backend.starts(),
        2,
        "the start and the restart that panicked"
    );

    // The session starts again after it.
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
}

/// A tap that never delivers anything (permission denied, a muted mix) is
/// reported through `system_lane_silent` and the level stream's floor,
/// while the recording itself completes.
#[test]
fn a_silent_system_lane_is_reported_in_statistics_and_levels() {
    let directory = tempfile::tempdir().unwrap();
    let signals: BTreeMap<AudioLane, SyntheticLane> = [
        (AudioLane::Mic, SyntheticLane::tone(440.0)),
        (AudioLane::System, SyntheticLane::SILENCE),
    ]
    .into_iter()
    .collect();
    let backend = Arc::new(SyntheticCaptureBackend::new(SyntheticOptions::signals(
        signals, 1.0,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let levels = session.levels();
    session.start(Uuid::new_v4()).unwrap();
    let first = levels.recv_timeout(RECV).unwrap();
    assert!((first.mic.rms - -9.03).abs() < 0.2);
    assert_eq!(first.system, Some(LaneLevel::SILENCE));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert!(result.statistics.system_lane_silent);
    assert!(result.statistics.dropped_frames.is_empty());
    let master = master_of(&result);
    assert!(master.channels[1].iter().all(|s| *s == 0.0));
    assert!(master.channels[0].iter().any(|s| *s != 0.0));
}

/// The default relay rides out a writer stalled for 20 s: every frame of
/// three channels (two lanes and the raw microphone) fits with nothing
/// draining it, so a slow sync or a sleeping disk drops nothing.
#[test]
fn the_default_relay_holds_twenty_seconds_with_the_writer_stalled() {
    let relay = FrameRelay::new(
        3,
        FRAME_SIZE,
        CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
    );
    let zeros = vec![0.0f32; FRAME_SIZE];
    for frame in 0..20 * FRAMES_PER_SECOND {
        assert!(relay.begin_frame(), "frame {frame} refused");
        for channel in 0..3 {
            relay.write(channel, &zeros);
        }
        relay.end_frame();
    }
    assert_eq!(relay.dropped_frames(), [0, 0, 0]);
}

/// The real writer on a filesystem where every periodic sync fails, the
/// plain `fsync` the Mac falls back to as well; with `close_too` the
/// close's sync fails too, after the real close has written everything.
struct SyncRefused {
    writer: RecordingWriter,
    close_too: bool,
}

impl RecordingWriting for SyncRefused {
    fn files(&self) -> RecordingFiles {
        self.writer.files()
    }
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        self.writer.write(frames)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        let files = self.writer.finish()?;
        if self.close_too {
            return Err(CaptureError::WriterFailed("the close's sync failed".into()));
        }
        Ok(files)
    }
}

/// A session over [`SyncRefused`] writers.
fn sync_refused_session(
    mode: CaptureMode,
    directory: &Path,
    backend: Arc<SyntheticCaptureBackend>,
    clock: Arc<dyn Clock>,
    close_too: bool,
) -> CaptureSession {
    CaptureSession::with_writer_factory(
        configuration(mode, directory, false),
        backend,
        passthrough(),
        CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
        clock,
        Arc::new(move |layout, lanes, keep_raw| {
            Ok(Box::new(SyncRefused {
                writer: RecordingWriter::new(layout, lanes, keep_raw)?,
                close_too,
            }) as Box<dyn RecordingWriting>)
        }),
    )
    .unwrap()
}

/// A sync that always fails cuts nothing short: 8 s delivered (past the
/// first sync, at 5 s) are all in the master, and the stop hands the
/// recording back. With nothing else ending the recording it ends `Failed`
/// with that recording on the first sync's error, since what was written
/// may not be on disk; a close that fails as well comes first.
#[test]
fn a_failing_sync_keeps_the_whole_recording_and_says_so() {
    for (close_too, expected) in [(false, "unsupported"), (true, "the close's sync failed")] {
        let directory = tempfile::tempdir().unwrap();
        let backend = Arc::new(SyntheticCaptureBackend::new(tones(
            &[AudioLane::Mixed],
            8.0,
        )));
        let session = sync_refused_session(
            CaptureMode::InPerson,
            directory.path(),
            backend.clone(),
            Arc::new(SystemClock::new()),
            close_too,
        );
        let states = session.states();
        session.start(Uuid::new_v4()).unwrap();
        backend.wait_until_finished();
        let result = session.stop().unwrap();
        let seen = collect_states(&states, until_failed);
        match seen.last() {
            Some(CaptureState::Failed {
                error: CaptureError::WriterFailed(detail),
                recording,
            }) => {
                assert!(detail.ends_with(expected), "{detail}");
                assert_eq!(recording.as_deref(), Some(&result));
            }
            _ => panic!("expected Failed(WriterFailed): {:?}", kinds(&seen)),
        }
        assert_eq!(master_of(&result).frame_count(), 800 * FRAME_SIZE);
        assert!(
            result.statistics.dropped_frames.values().all(|n| *n == 0),
            "{:?}",
            result.statistics.dropped_frames
        );
    }
}

/// A failed sync never stands in for what ended the recording: syncs
/// refused from the first one, at 5 s, then a device lost for good at 6 s
/// end `DeviceLost`, with all 6 s in the master.
#[test]
fn refused_syncs_then_a_device_loss_end_device_lost() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(6.0)
            .restarts_that_fail(CaptureSession::RESTART_ATTEMPTS),
    ));
    let session = sync_refused_session(
        CaptureMode::Call,
        directory.path(),
        backend,
        clock.clone(),
        false,
    );
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, CaptureSession::RESTART_ATTEMPTS - 1);
    let seen = collect_states(&states, until_failed);
    let result = session.stop().unwrap();
    assert_eq!(
        *seen.last().unwrap(),
        CaptureState::Failed {
            error: CaptureError::DeviceLost,
            recording: Some(Box::new(result.clone()))
        }
    );
    assert!(result.statistics.ended_on_device_loss);
    assert_eq!(master_of(&result).frame_count(), 600 * FRAME_SIZE);
}

/// One frame of relay headroom against a backend that delivers two seconds
/// in milliseconds: the writer falls behind, and every frame it missed is
/// counted against the master that was written, on every lane alike.
#[test]
fn dropped_frames_account_for_every_frame_the_writer_missed() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 2.0)));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    let master = master_of(&result);
    let processed_frames = 200;
    for lane in call() {
        let dropped = result
            .statistics
            .dropped_frames
            .get(&lane)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            dropped + master.frame_count() / 480,
            processed_frames,
            "{}: {dropped} dropped + {} written",
            lane.as_str(),
            master.frame_count() / 480
        );
    }
    assert_eq!(
        result.statistics.duration,
        master.frame_count() as f64 / 48_000.0
    );
    let sidecar = sidecar_of(&result, AudioLane::Mic);
    assert_eq!(
        sidecar.len(),
        master.frame_count() / 3,
        "sidecars drop with the master"
    );
}

#[test]
fn stop_after_device_loss_returns_the_same_recording_every_time() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(0.5)
            .restarts_that_fail(CaptureSession::RESTART_ATTEMPTS),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        clock.clone(),
    )
    .unwrap();
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, CaptureSession::RESTART_ATTEMPTS - 1);
    collect_states(&states, until_failed);
    let first = session.stop().unwrap();
    let second = session.stop().unwrap();
    assert_eq!(first, second);
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::DeviceLost,
            recording: Some(Box::new(first.clone()))
        }
    );
    backend.stop();
    backend.stop();
    assert_eq!(
        master_of(&first).frame_count(),
        (first.statistics.duration * 48_000.0).round() as usize
    );
}

#[test]
fn in_person_produces_one_channel_and_the_mixed_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        1.0,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let levels = session.levels();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.asset.lanes, vec![AudioLane::Mixed]);
    assert_eq!(
        result
            .asset
            .sidecars_16k
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![AudioLane::Mixed]
    );
    assert_eq!(master_of(&result).channels.len(), 1);
    assert!(
        !result.statistics.system_lane_silent,
        "no system lane, so not 'silent'"
    );
    let latest = levels.recv_timeout(RECV).unwrap();
    assert_eq!(latest.system, None);
}

#[test]
fn raw_mic_lane_is_kept_when_asked() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 0.5)));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), true),
        backend.clone(),
        passthrough(),
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    let raw = RecordingLayout::from_asset(&result.asset)
        .unwrap()
        .directory
        .join("mic.raw.caf");
    assert!(raw.exists());
    assert_eq!(
        CafFile::read(&raw).unwrap().channels[0],
        master_of(&result).channels[0]
    );
}

#[test]
fn start_while_recording_and_stop_while_idle_fail() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        1.0,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend,
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    assert!(matches!(session.stop(), Err(CaptureError::InvalidState(_))));
    session.start(Uuid::new_v4()).unwrap();
    assert!(matches!(
        session.start(Uuid::new_v4()),
        Err(CaptureError::InvalidState(_))
    ));
    session.stop().unwrap();
}

/// Fifty start/stop cycles on one session: every cycle ends idle with a
/// readable master and nothing carries over (rings cleared, threads
/// joined).
#[test]
fn repeated_start_stop_cycles_stay_clean() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 0.1)));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    for cycle in 0..50 {
        session.start(Uuid::new_v4()).unwrap();
        backend.wait_until_finished();
        let result = session.stop().unwrap();
        assert_eq!(session.state(), CaptureState::Idle);
        assert!(
            (result.statistics.duration - 0.1).abs() < 0.02,
            "cycle {cycle}"
        );
        assert!(result.statistics.dropped_frames.is_empty(), "cycle {cycle}");
        assert_eq!(master_of(&result).channels.len(), 2);
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 50);
}

/// A backend that records once and fails on every later start.
struct OnceThenFailing {
    inner: Arc<SyntheticCaptureBackend>,
    starts: AtomicUsize,
}

impl CaptureBackend for OnceThenFailing {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        if self.starts.fetch_add(1, Ordering::Relaxed) != 0 {
            return Err(CaptureError::InputDeviceUnavailable);
        }
        self.inner.start(lanes, uid, sink)
    }
    fn stop(&self) {
        self.inner.stop();
    }
}

/// A backend that keeps the live backends' promise for a chosen
/// microphone (`CaptureBackend::start`): while `connected` it records the
/// one asked for, otherwise the default input in its place, saying so in
/// the stream. A chosen one that is connected but not `opens` fails the
/// start, as a device still settling or held elsewhere does, with
/// [`Self::chosen_failure`] (or, when it `stalls`, as one linked that never
/// runs); the default fails while not `default_opens`, with
/// [`Self::default_failure`]. With `default_is_chosen` the default input is
/// the chosen microphone itself. Every UID it was asked for is kept. With
/// `probes` it can ask a microphone on a stream of its own, as the Linux
/// backend does: the chosen one delivers there while connected and
/// `opens`; every probe is counted.
struct ChosenOrDefault {
    inner: SyntheticCaptureBackend,
    connected: AtomicBool,
    opens: AtomicBool,
    stalls: AtomicBool,
    default_opens: AtomicBool,
    default_is_chosen: AtomicBool,
    asked: Mutex<Vec<Option<String>>>,
    probes: AtomicBool,
    probed: AtomicUsize,
}

impl ChosenOrDefault {
    const CHOSEN: &str = "usb-microphone";

    fn new(seconds: f64) -> Self {
        Self::over(tones(&[AudioLane::Mixed], seconds))
    }

    /// Delivering at wall-clock speed and watched by the session, as a
    /// live backend is.
    fn watched(seconds: f64) -> Self {
        Self::over(
            tones(&[AudioLane::Mixed], seconds)
                .real_time(true)
                .delivers_continuously(true),
        )
    }

    fn over(options: SyntheticOptions) -> Self {
        Self {
            inner: SyntheticCaptureBackend::new(options),
            connected: AtomicBool::new(true),
            opens: AtomicBool::new(true),
            stalls: AtomicBool::new(false),
            default_opens: AtomicBool::new(true),
            default_is_chosen: AtomicBool::new(false),
            asked: Mutex::new(Vec::new()),
            probes: AtomicBool::new(false),
            probed: AtomicUsize::new(0),
        }
    }

    fn chosen_failure() -> CaptureError {
        CaptureError::BackendFailed("the chosen microphone did not open".into())
    }

    fn default_failure() -> CaptureError {
        CaptureError::BackendFailed("the default input did not open".into())
    }

    /// The chosen microphone and, after the first start on it, every UID
    /// asked for: `restarts` more for it, then the default.
    fn chosen_then_default(restarts: usize) -> Vec<Option<String>> {
        let mut asked = vec![Some(Self::CHOSEN.to_owned()); 1 + restarts];
        asked.push(None);
        asked
    }

    /// A session asking this backend for [`Self::CHOSEN`]. The relay holds
    /// 10 s, so a gap written on a manual clock leaves room for what the
    /// restarted backend delivered at once.
    fn session(self: &Arc<Self>, directory: &Path, clock: Arc<dyn Clock>) -> CaptureSession {
        let mut configuration = configuration(CaptureMode::InPerson, directory, false);
        configuration.input_device_uid = Some(Self::CHOSEN.to_owned());
        CaptureSession::with_backend(configuration, self.clone(), None, 1_000, clock).unwrap()
    }

    fn asked(&self) -> Vec<Option<String>> {
        self.asked.lock().unwrap().clone()
    }

    fn input(uid: &str, name: &str, is_fallback: bool) -> Option<CaptureInput> {
        Some(CaptureInput {
            uid: uid.to_owned(),
            name: Some(name.to_owned()),
            is_fallback,
        })
    }

    /// The stream's input on the chosen microphone.
    fn chosen() -> Option<CaptureInput> {
        Self::input(Self::CHOSEN, "USB Microphone", false)
    }

    /// The stream's input on the default input, recorded as the fallback.
    fn fallback() -> Option<CaptureInput> {
        Self::input("built-in", "Built-in Microphone", true)
    }
}

impl CaptureBackend for ChosenOrDefault {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        self.asked.lock().unwrap().push(uid.map(str::to_owned));
        let on_chosen = uid.is_some() && self.connected.load(Ordering::Relaxed);
        if on_chosen && !self.opens.load(Ordering::Relaxed) {
            return Err(if self.stalls.load(Ordering::Relaxed) {
                CaptureError::DidNotRun("the graph did not run".into())
            } else {
                Self::chosen_failure()
            });
        }
        if !on_chosen && !self.default_opens.load(Ordering::Relaxed) {
            return Err(Self::default_failure());
        }
        let stream = self.inner.start(lanes, uid, sink)?;
        let input = match uid {
            Some(uid) if on_chosen => Self::input(uid, "USB Microphone", false),
            None if self.default_is_chosen.load(Ordering::Relaxed) => Self::chosen(),
            _ => Self::input("built-in", "Built-in Microphone", uid.is_some()),
        };
        Ok(CaptureStream { input, ..stream })
    }

    fn stop(&self) {
        self.inner.stop();
    }

    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        self.inner.delivers_continuously(lanes)
    }

    fn waits_for_playback(&self, lanes: &[AudioLane]) -> bool {
        self.inner.waits_for_playback(lanes)
    }

    fn probes_inputs(&self) -> bool {
        self.probes.load(Ordering::Relaxed)
    }

    fn probe_input(&self, uid: &str) -> bool {
        assert_eq!(uid, Self::CHOSEN);
        let delivers = self.connected.load(Ordering::Relaxed) && self.opens.load(Ordering::Relaxed);
        self.probed.fetch_add(1, Ordering::SeqCst);
        delivers
    }
}

/// A chosen microphone that goes during a recording: the rebuild asks for
/// it again, the backend records the default input in its place, and the
/// recording goes on, without `DeviceLost`, its stream naming the fallback;
/// once the microphone is back, the next change returns to it.
#[test]
fn a_chosen_microphone_that_goes_and_comes_back_keeps_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    let session = backend.session(directory.path(), Arc::new(SystemClock::new()));
    let notices = session.notices();
    let next_notice = || notices.recv_timeout(Duration::from_secs(5)).unwrap();
    let input = || session.stream().and_then(|stream| stream.input);
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(input(), ChosenOrDefault::chosen());

    backend.connected.store(false, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::InputDeviceGone);
    assert_eq!(
        next_notice(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::InputDeviceGone)
    );
    assert!(matches!(
        next_notice(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    assert_eq!(
        input(),
        ChosenOrDefault::fallback(),
        "the default input as the fallback"
    );

    backend.connected.store(true, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        next_notice(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(matches!(
        next_notice(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(
        input(),
        ChosenOrDefault::chosen(),
        "back on the chosen microphone"
    );

    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 2);
    assert_eq!(
        backend.asked(),
        vec![Some(ChosenOrDefault::CHOSEN.to_owned()); 3],
        "every start asks for the chosen microphone"
    );
}

/// The chosen microphone comes back (a change) while the recording is on
/// the fallback, but does not open: the default worked a moment ago, so it
/// is tried right after the first failed restart, not after the last, and
/// the recording goes on there, marked as the fallback, after a gap of one
/// restart rather than four.
#[test]
fn a_chosen_microphone_that_comes_back_but_does_not_open_keeps_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    backend.connected.store(false, Ordering::Relaxed);
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    let on_the_fallback = ChosenOrDefault::fallback();
    assert_eq!(session.stream().and_then(|s| s.input), on_the_fallback);

    backend.connected.store(true, Ordering::Relaxed);
    backend.opens.store(false, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        on_the_fallback,
        "the default input, marked as the fallback"
    );
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(1));

    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 1);
}

/// The chosen microphone records, then stops opening at a rebuild: it is
/// asked for through every restart, since a device back within the backoff
/// keeps its place, and only after the last is the default started.
#[test]
fn a_chosen_microphone_that_stops_opening_is_retried_before_the_default() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();

    backend.opens.store(false, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, CaptureSession::RESTART_ATTEMPTS - 1);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == CaptureSession::RESTART_ATTEMPTS
    ));
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::fallback(),
        "the default input, marked as the fallback"
    );
    assert_eq!(
        backend.asked(),
        ChosenOrDefault::chosen_then_default(CaptureSession::RESTART_ATTEMPTS)
    );
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
}

/// A chosen microphone that is linked but does not run
/// ([`CaptureError::DidNotRun`]) at a rebuild: each such restart waits out
/// the backend's start deadline, so the default is tried after the first.
/// When the default does not open either, the restarts go on, and the last
/// one tries the default again.
#[test]
fn a_chosen_microphone_that_does_not_run_is_replaced_after_one_restart() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();

    backend.opens.store(false, Ordering::Relaxed);
    backend.stalls.store(true, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(1));

    backend.default_opens.store(false, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 1);
    backend.default_opens.store(true, Ordering::Relaxed);
    for step in &CaptureSession::RESTART_BACKOFF[1..] {
        assert!(clock.wait_for_sleepers(1), "the next backoff");
        clock.advance(*step);
    }
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == CaptureSession::RESTART_ATTEMPTS
    ));
    let mut asked = ChosenOrDefault::chosen_then_default(1);
    asked.extend([Some(ChosenOrDefault::CHOSEN.to_owned()), None]);
    asked.extend(ChosenOrDefault::chosen_then_default(
        CaptureSession::RESTART_ATTEMPTS - 2,
    ));
    assert_eq!(backend.asked(), asked);
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 2);
}

/// A chosen microphone whose first restart fails to open and whose second
/// does not run ([`CaptureError::DidNotRun`]): the default is tried right
/// after the second, not only after a first.
#[test]
fn a_chosen_microphone_that_does_not_run_at_a_later_restart_is_replaced_there() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();

    backend.opens.store(false, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(clock.wait_for_sleepers(1), "the first restart failed");
    backend.stalls.store(true, Ordering::Relaxed);
    clock.advance(CaptureSession::RESTART_BACKOFF[0]);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 2, .. }
    ));
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(2));
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::fallback(),
        "the default input, marked as the fallback"
    );
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
}

/// A chosen microphone that is connected but does not open at the start:
/// the recording starts on the default input, marked as the fallback. Only
/// when the default does not open either does the start fail, with the
/// chosen one's error.
#[test]
fn a_chosen_microphone_that_does_not_open_at_the_start_records_the_default() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(ChosenOrDefault::new(0.2));
    backend.opens.store(false, Ordering::Relaxed);
    let session = backend.session(directory.path(), Arc::new(SystemClock::new()));
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::fallback()
    );
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(0));
    backend.inner.wait_until_finished();
    assert!(!session.stop().unwrap().statistics.ended_on_device_loss);

    backend.default_opens.store(false, Ordering::Relaxed);
    assert_eq!(
        session.start(Uuid::new_v4()),
        Err(ChosenOrDefault::chosen_failure()),
        "the chosen microphone's error, not the default's"
    );
    assert!(matches!(
        session.state(),
        CaptureState::Failed {
            recording: None,
            ..
        }
    ));
}

/// A chosen microphone that is also the default input, whose start by its
/// UID failed: the start without a UID records it, and it is not marked as
/// the fallback, so no warning says it is missing.
#[test]
fn a_chosen_microphone_that_is_the_default_is_not_its_own_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(ChosenOrDefault::new(0.2));
    backend.opens.store(false, Ordering::Relaxed);
    backend.default_is_chosen.store(true, Ordering::Relaxed);
    let session = backend.session(directory.path(), Arc::new(SystemClock::new()));
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::chosen()
    );
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(0));
    backend.inner.wait_until_finished();
    assert!(!session.stop().unwrap().statistics.ended_on_device_loss);
}

/// Meeting A records and stops; meeting B's start fails in the backend.
/// `stop()` after that failure must fail, not hand out A's asset under B's
/// meeting (the app would enqueue A twice).
#[test]
fn a_failed_start_does_not_return_the_previous_meetings_recording() {
    let directory = tempfile::tempdir().unwrap();
    let inner = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.2,
    )));
    let backend = Arc::new(OnceThenFailing {
        inner: inner.clone(),
        starts: AtomicUsize::new(0),
    });
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend,
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let first = Uuid::new_v4();
    session.start(first).unwrap();
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.asset.meeting_id, first);

    assert_eq!(
        session.start(Uuid::new_v4()),
        Err(CaptureError::InputDeviceUnavailable)
    );
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::InputDeviceUnavailable,
            recording: None
        }
    );
    assert!(
        matches!(session.stop(), Err(CaptureError::InvalidState(_))),
        "nothing was recorded for this start"
    );
}

/// Logs `reset` and `process` calls in order.
struct ResetLoggingCanceller {
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl EchoCanceller for ResetLoggingCanceller {
    fn process(&mut self, near_end: &[f32], _far_end: &[f32], out: &mut [f32]) {
        let mut log = self.log.lock().unwrap();
        if log.last() != Some(&"process") {
            log.push("process");
        }
        let count = near_end.len().min(out.len());
        out[..count].copy_from_slice(&near_end[..count]);
    }
    fn reset(&mut self) {
        self.log.lock().unwrap().push("reset");
    }
}

/// One canceller serves every recording of a session, so each start resets
/// it before the first frame: meeting two on headphones must not begin with
/// the filter meeting one converged on the loudspeakers.
#[test]
fn every_start_resets_the_echo_canceller_before_the_first_frame() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 0.1)));
    let log = Arc::new(Mutex::new(Vec::new()));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        Some(Box::new(ResetLoggingCanceller { log: log.clone() })),
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    for _ in 0..2 {
        session.start(Uuid::new_v4()).unwrap();
        backend.wait_until_finished();
        session.stop().unwrap();
    }
    assert_eq!(
        *log.lock().unwrap(),
        ["reset", "process", "reset", "process"]
    );
}

/// Frame counts move between rates rounded down: a device's to 48 kHz,
/// and a microphone on its own clock to the stream's; a rate that could
/// not be read leaves the count alone.
#[test]
fn frame_counts_are_rescaled_between_rates_rounding_down() {
    let hands_free = CaptureStream {
        sample_rate: 24_000.0,
        ..CaptureStream::SYNTHETIC
    };
    assert_eq!(hands_free.at_output_rate(240 + 4_800), 10_080);
    assert_eq!(CaptureStream::SYNTHETIC.at_output_rate(5_041), 5_041);
    // 1 000 * 48 000 / 44 100 = 1 088.4.
    let consumer = CaptureStream {
        sample_rate: 44_100.0,
        ..CaptureStream::SYNTHETIC
    };
    assert_eq!(consumer.at_output_rate(1_000), 1_088);
    // A 48 kHz microphone beside a 24 kHz clock master.
    assert_eq!(CaptureStream::rescaled(481, 48_000.0, 24_000.0), 240);
    assert_eq!(CaptureStream::rescaled(481, 0.0, 24_000.0), 481);
    assert_eq!(CaptureStream::rescaled(481, f64::NAN, 24_000.0), 481);
}

/// The far-end is delayed by both device paths whenever their sum reaches
/// one processing frame; the Speex tail keeps the room.
#[test]
fn far_end_delay_covers_input_and_output_paths_from_one_frame_up() {
    assert_eq!(CaptureSession::far_end_delay_frames(0, 0), 0);
    assert_eq!(
        CaptureSession::far_end_delay_frames(200, 200),
        0,
        "under one frame the tail absorbs it"
    );
    assert_eq!(CaptureSession::far_end_delay_frames(300, 300), 600);
    assert_eq!(
        CaptureSession::far_end_delay_frames(1_440, 9_600),
        11_040,
        "a 30 ms mic path plus a 200 ms Bluetooth output"
    );
    assert_eq!(CaptureSession::far_end_delay_frames(0, 480), 480);
    assert_eq!(CaptureSession::gap_frames(Duration::ZERO), 0);
    assert_eq!(CaptureSession::gap_frames(Duration::from_millis(255)), 25);
    assert_eq!(CaptureSession::gap_frames(Duration::from_secs(10)), 1_000);
}

#[test]
fn the_session_exposes_the_backends_stream_while_recording() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.1,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    assert_eq!(session.stream(), None);
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(session.stream(), Some(CaptureStream::SYNTHETIC));
    backend.wait_until_finished();
    session.stop().unwrap();
    assert_eq!(session.stream(), None);
}

/// Wraps the real writer and fails where a full disk would: every `write`
/// after `fail_after_frames`, and `finish()` itself when asked.
struct FaultyWriter {
    inner: RecordingWriter,
    fail_after_frames: Option<usize>,
    fail_finish: bool,
    frames: usize,
}

impl RecordingWriting for FaultyWriter {
    fn files(&self) -> RecordingFiles {
        self.inner.files()
    }
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        self.frames += 1;
        if let Some(limit) = self.fail_after_frames
            && self.frames > limit
        {
            return Err(CaptureError::WriterFailed("DiskFull".into()));
        }
        self.inner.write(frames)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.sync()
    }
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        let files = self.inner.finish()?;
        if self.fail_finish {
            return Err(CaptureError::WriterFailed("DiskFull".into()));
        }
        Ok(files)
    }
}

fn faulty_session(
    directory: &Path,
    backend: Arc<dyn CaptureBackend>,
    fail_after_frames: Option<usize>,
    fail_finish: bool,
    clock: Arc<dyn Clock>,
) -> CaptureSession {
    CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory, false),
        backend,
        None,
        1_000,
        clock,
        Arc::new(move |layout, lanes, keep_raw| {
            Ok(Box::new(FaultyWriter {
                inner: RecordingWriter::new(layout, lanes, keep_raw)?,
                fail_after_frames,
                fail_finish,
                frames: 0,
            }) as Box<dyn RecordingWriting>)
        }),
    )
    .unwrap()
}

/// The disk fills while closing the files: `stop()` still returns the
/// asset and the state, not an error, carries the failure.
#[test]
fn a_failing_finish_still_returns_the_asset_and_ends_failed() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let session = faulty_session(
        directory.path(),
        backend.clone(),
        None,
        true,
        Arc::new(SystemClock::new()),
    );
    let meeting_id = Uuid::new_v4();
    session.start(meeting_id).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.asset.meeting_id, meeting_id);
    assert!((result.statistics.duration - 0.5).abs() < 0.02);
    assert_eq!(master_of(&result).frame_count(), 24_000);
    match session.state() {
        CaptureState::Failed {
            error: CaptureError::WriterFailed(detail),
            recording,
        } => {
            assert!(detail.contains("DiskFull"));
            assert_eq!(
                recording.as_deref(),
                Some(&result),
                "the failed state carries the recording"
            );
        }
        other => panic!("expected Failed(WriterFailed), got {other:?}"),
    }
    assert_eq!(
        session.stop().unwrap(),
        result,
        "and stop() returns the same one"
    );
}

/// The disk fills mid-recording: the first write error stops the writes,
/// the session finalises, and `stop()` returns what was written.
#[test]
fn a_failing_write_mid_recording_finalises_what_was_written() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        2.0,
    )));
    let session = faulty_session(
        directory.path(),
        backend.clone(),
        Some(30),
        false,
        Arc::new(SystemClock::new()),
    );
    let states = session.states();
    session.start(Uuid::new_v4()).unwrap();
    let seen = collect_states(&states, until_failed);
    assert!(
        matches!(
            seen.last(),
            Some(CaptureState::Failed {
                error: CaptureError::WriterFailed(_),
                ..
            })
        ),
        "{seen:?}"
    );
    let result = session.stop().unwrap();
    assert!((result.statistics.duration - 0.3).abs() < 0.001);
    assert_eq!(master_of(&result).frame_count(), 30 * 480);
}

/// A writer failure names its cause once from each place the session
/// reports one: a writer that cannot open its folder, a failed close and a
/// failed write, each read as `Display`.
#[test]
fn a_writer_failure_says_writing_the_recording_failed_once() {
    let once = |error: &CaptureError| {
        let text = error.to_string();
        assert_eq!(
            text.matches("writing the recording failed").count(),
            1,
            "{text}"
        );
        text
    };

    // The output directory is a file, so the meeting's folder cannot be made.
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("a-file");
    std::fs::write(&blocked, b"").unwrap();
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, &blocked, false),
        Arc::new(SyntheticCaptureBackend::new(tones(
            &[AudioLane::Mixed],
            0.1,
        ))),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let meeting_id = Uuid::new_v4();
    let opened = once(&session.start(meeting_id).unwrap_err());
    let folder = RecordingLayout::new(&blocked, meeting_id).directory;
    assert!(
        opened.starts_with(&format!(
            "writing the recording failed: {}: ",
            folder.display()
        )),
        "{opened}"
    );

    let failed_error = |session: &CaptureSession| match session.state() {
        CaptureState::Failed { error, .. } => error,
        other => panic!("expected Failed, got {other:?}"),
    };
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let closing = faulty_session(
        directory.path(),
        backend.clone(),
        None,
        true,
        Arc::new(SystemClock::new()),
    );
    closing.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    closing.stop().unwrap();
    assert_eq!(
        once(&failed_error(&closing)),
        "writing the recording failed: DiskFull"
    );

    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let writing = faulty_session(
        directory.path(),
        backend.clone(),
        Some(5),
        false,
        Arc::new(SystemClock::new()),
    );
    let states = writing.states();
    writing.start(Uuid::new_v4()).unwrap();
    collect_states(&states, until_failed);
    assert_eq!(
        once(&failed_error(&writing)),
        "writing the recording failed: DiskFull"
    );
}

/// The writer thread panics on its tenth frame and takes the writer with
/// it: `stop()` fails with `WriterFailed` instead of leaving the session
/// stuck in `Stopping`, and the next recording starts.
#[test]
fn a_writer_thread_that_died_ends_failed_and_the_session_starts_again() {
    struct Panicking(RecordingWriter, usize);
    impl RecordingWriting for Panicking {
        fn files(&self) -> RecordingFiles {
            self.0.files()
        }
        fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            self.1 += 1;
            assert!(self.1 < 10, "writer panics on purpose");
            self.0.write(frames)
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.0.sync()
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            self.0.finish()
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let first = Arc::new(AtomicBool::new(true));
    let session = CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        1_000,
        Arc::new(SystemClock::new()),
        Arc::new(move |layout, lanes, keep_raw| {
            let writer = RecordingWriter::new(layout, lanes, keep_raw)?;
            Ok(if first.swap(false, Ordering::SeqCst) {
                Box::new(Panicking(writer, 0)) as Box<dyn RecordingWriting>
            } else {
                Box::new(writer)
            })
        }),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let error = session.stop().unwrap_err();
    assert!(matches!(error, CaptureError::WriterFailed(_)), "{error:?}");
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error,
            recording: None
        }
    );
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    assert_eq!(master_of(&session.stop().unwrap()).frame_count(), 24_000);
    assert_eq!(session.state(), CaptureState::Idle);
}

/// A write fails and no thread can be spawned to end the recording on
/// (refused here, as when the system has none to give): the state stays
/// `Recording` over a writer that drops every frame, `write_failed` says
/// so for a watcher to stop it, and `stop()` returns the recording with
/// the write's failure in it.
#[test]
fn a_failed_write_with_no_thread_to_end_it_shows_through_write_failed() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let session = faulty_session(
        directory.path(),
        backend.clone(),
        Some(5),
        false,
        Arc::new(SystemClock::new()),
    );
    session.refuse_the_writer_failure_thread();
    assert!(!session.write_failed());
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !session.write_failed() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(session.write_failed(), "the failed write shows");
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    let result = session.stop().unwrap();
    assert!(
        matches!(&result.failure, Some(CaptureError::WriterFailed(detail)) if detail.contains("DiskFull")),
        "{:?}",
        result.failure
    );
    assert_eq!(master_of(&result).frame_count(), 5 * FRAME_SIZE);
    assert!(!session.write_failed());
}

/// A start that panics part way (here its writer factory; in the product a
/// writer or a processing thread the system cannot spawn) leaves `Failed`
/// with no recording rather than `Starting` for good, and the next start
/// records.
#[test]
fn a_start_that_panics_ends_failed_and_the_session_starts_again() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let first = Arc::new(AtomicBool::new(true));
    let session = CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        1_000,
        Arc::new(SystemClock::new()),
        Arc::new(move |layout, lanes, keep_raw| {
            assert!(
                !first.swap(false, Ordering::SeqCst),
                "the first start panics on purpose"
            );
            Ok(Box::new(RecordingWriter::new(layout, lanes, keep_raw)?)
                as Box<dyn RecordingWriting>)
        }),
    )
    .unwrap();
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.start(Uuid::new_v4())
    }));
    assert!(started.is_err(), "the start panicked");
    assert!(
        matches!(
            session.state(),
            CaptureState::Failed {
                error: CaptureError::BackendFailed(_),
                recording: None
            }
        ),
        "{:?}",
        session.state()
    );
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    assert_eq!(master_of(&session.stop().unwrap()).frame_count(), 24_000);
    assert_eq!(session.state(), CaptureState::Idle);
}

/// A start that panics once the backend runs (here the backend's own
/// `start`, its producer thread already spawned) stops the backend on the
/// way to `Failed`: the next start finds it free and records.
#[test]
fn a_start_that_panics_once_the_backend_runs_stops_it() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 0.5).start_panics_once_running(),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.start(Uuid::new_v4())
    }));
    assert!(started.is_err(), "the start panicked");
    assert!(
        matches!(
            session.state(),
            CaptureState::Failed {
                error: CaptureError::BackendFailed(_),
                recording: None
            }
        ),
        "{:?}",
        session.state()
    );
    // A backend left running refuses this start ("already started").
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(backend.starts(), 2);
    backend.wait_until_finished();
    assert_eq!(master_of(&session.stop().unwrap()).frame_count(), 24_000);
}

/// The meeting's folder is deleted while recording. On Unix the open files
/// keep writing and close without an error, so only the missing master
/// tells: `stop()` fails with `WriterFailed` rather than returning an asset
/// whose files are gone.
#[cfg(unix)]
#[test]
fn a_folder_deleted_while_recording_ends_writer_failed() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.5,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let meeting_id = Uuid::new_v4();
    session.start(meeting_id).unwrap();
    backend.wait_until_finished();
    std::fs::remove_dir_all(RecordingLayout::new(directory.path(), meeting_id).directory).unwrap();
    let error = session.stop().unwrap_err();
    match &error {
        CaptureError::WriterFailed(detail) => assert!(detail.ends_with("is gone"), "{detail}"),
        other => panic!("expected WriterFailed, got {other:?}"),
    }
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error,
            recording: None
        }
    );
}

/// Stands in for the IOProc: the test pushes callbacks into the sink the
/// session handed over.
struct HandsOverTheSink {
    sink: Mutex<Option<Arc<LaneFrameSink>>>,
    stream: CaptureStream,
    /// What every start after the first reports, when it should differ.
    stream_after_restart: Option<CaptureStream>,
}

impl HandsOverTheSink {
    fn new(stream: CaptureStream) -> Self {
        Self {
            sink: Mutex::new(None),
            stream,
            stream_after_restart: None,
        }
    }
}

impl CaptureBackend for HandsOverTheSink {
    fn start(
        &self,
        _: &[AudioLane],
        _: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let restarted = self.sink.lock().unwrap().replace(sink).is_some();
        Ok(match &self.stream_after_restart {
            Some(stream) if restarted => stream.clone(),
            _ => self.stream.clone(),
        })
    }
    fn stop(&self) {}
}

/// Passes the near end through once the gate opens; until then the
/// processing thread is stuck in its first frame.
struct GatedCanceller {
    open: Arc<(Mutex<bool>, Condvar)>,
}

impl EchoCanceller for GatedCanceller {
    fn process(&mut self, near_end: &[f32], _far_end: &[f32], out: &mut [f32]) {
        let (open, condvar) = &*self.open;
        let mut guard = open.lock().unwrap();
        while !*guard {
            guard = condvar.wait(guard).unwrap();
        }
        let count = near_end.len().min(out.len());
        out[..count].copy_from_slice(&near_end[..count]);
    }
    fn reset(&mut self) {}
}

/// Three seconds of callbacks while the processing thread is stuck: the
/// two-second rings take what fits (plus what the thread read before it
/// stuck), refuse the rest and count it, and those overruns reach
/// `dropped_frames`.
#[test]
fn ring_overruns_are_reported_in_dropped_frames() {
    ring_overruns_are_reported(SAMPLE_RATE);
}

/// The same at 24 kHz: a refused 480-sample callback is two 48 kHz frames.
#[test]
fn ring_overruns_at_24_khz_are_reported_in_48_khz_frames() {
    ring_overruns_are_reported(24_000.0);
}

/// The two tests above, with the rings at `rate`.
fn ring_overruns_are_reported(rate: f64) {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(HandsOverTheSink::new(CaptureStream {
        sample_rate: rate,
        ..CaptureStream::SYNTHETIC
    }));
    let open = Arc::new((Mutex::new(false), Condvar::new()));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        Some(Box::new(GatedCanceller { open: open.clone() })),
        2_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    let sink = backend.sink.lock().unwrap().clone().unwrap();
    let buffer = [0.25f32; 480];
    let mut accepted = 0;
    for _ in 0..300 {
        if sink.begin_callback(480) {
            sink.write_slice(0, &buffer);
            sink.write_slice(1, &buffer);
            sink.end_callback();
            accepted += 1;
        }
    }
    *open.0.lock().unwrap() = true;
    open.1.notify_all();
    let result = session.stop().unwrap();
    let refused = 300 - accepted;
    assert!(
        refused > 0 && accepted >= 200,
        "the rings hold two seconds: {accepted} accepted"
    );
    // 48 kHz frames per callback; at 24 kHz half a converter window is held
    // back, one frame less in the master.
    let frames = (SAMPLE_RATE / rate) as usize;
    let held = usize::from(rate != SAMPLE_RATE);
    assert_eq!(
        master_of(&result).frame_count(),
        (accepted * frames - held) * 480
    );
    let dropped = refused * frames;
    assert_eq!(
        result.statistics.dropped_frames,
        BTreeMap::from([(AudioLane::Mic, dropped), (AudioLane::System, dropped)])
    );
}

/// Overruns at 48 kHz, then a headset that switches to 16 kHz mid-call:
/// the overruns are counted at the rate of the stream they happened on, not
/// the rate the recording ends at (which would read them three times over).
#[test]
fn ring_overruns_before_a_rate_change_keep_their_rate() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(HandsOverTheSink {
        stream_after_restart: Some(CaptureStream {
            sample_rate: 16_000.0,
            ..CaptureStream::SYNTHETIC
        }),
        ..HandsOverTheSink::new(CaptureStream::SYNTHETIC)
    });
    let open = Arc::new((Mutex::new(false), Condvar::new()));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        Some(Box::new(GatedCanceller { open: open.clone() })),
        2_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    let sink = backend.sink.lock().unwrap().clone().unwrap();
    let buffer = [0.25f32; 480];
    let mut refused = 0;
    for _ in 0..400 {
        if sink.begin_callback(480) {
            sink.write_slice(0, &buffer);
            sink.write_slice(1, &buffer);
            sink.end_callback();
        } else {
            refused += 1;
        }
    }
    assert!(refused > 0, "the rings overran");
    *open.0.lock().unwrap() = true;
    open.1.notify_all();
    sink.report_device_change(DeviceChangeReason::SampleRateChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::SampleRateChanged)
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(session.stream().unwrap().sample_rate, 16_000.0);
    let result = session.stop().unwrap();
    // One refused 480-sample callback at 48 kHz is one frame.
    assert_eq!(
        result.statistics.dropped_frames,
        BTreeMap::from([(AudioLane::Mic, refused), (AudioLane::System, refused)])
    );
}

/// A backend that reports a rate the converter cannot take (the live
/// backends refuse one at `start`) is recorded unconverted, sample for
/// sample, instead of panicking with the session's lock held. 1.2 s at
/// 4 kHz is ten whole frames, so no remainder is left in the rings.
#[test]
fn a_stream_at_a_rate_the_converter_cannot_take_is_recorded_unconverted() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 1.2).stream(
        CaptureStream {
            sample_rate: 4_000.0,
            ..CaptureStream::SYNTHETIC
        },
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();

    assert_eq!(backend.frames_delivered(), 4_800);
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(master_of(&result).frame_count(), 4_800);
}

struct Failing;

impl CaptureBackend for Failing {
    fn start(
        &self,
        _: &[AudioLane],
        _: Option<&str>,
        _: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        Err(CaptureError::InputDeviceUnavailable)
    }
    fn stop(&self) {}
}

/// Every file under `folder`, by path, with its bytes.
fn tree(folder: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(tree(&path));
        } else {
            files.insert(path.clone(), std::fs::read(&path).unwrap());
        }
    }
    files
}

/// `audio` holds another meeting's folder with a master and a sidecar,
/// and a file of its own; the meeting's id.
fn audio_folder_with_a_meeting(audio: &Path) -> Uuid {
    let id = Uuid::new_v4();
    let other = RecordingLayout::new(audio, id);
    std::fs::create_dir_all(&other.directory).unwrap();
    std::fs::write(other.master(AudioFormat::Caf48kFloat32), b"another master").unwrap();
    std::fs::write(other.sidecar(AudioLane::Mixed), b"another sidecar").unwrap();
    std::fs::write(audio.join("notes.txt"), b"the user's").unwrap();
    id
}

/// A start whose devices do not open removes the folder it made and
/// nothing else: another meeting's folder and the audio folder stay.
#[test]
fn a_failing_backend_leaves_the_session_failed_and_no_folder() {
    let directory = tempfile::tempdir().unwrap();
    let audio = directory.path().join("audio");
    std::fs::create_dir(&audio).unwrap();
    audio_folder_with_a_meeting(&audio);
    let before = tree(&audio);
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, &audio, false),
        Arc::new(Failing),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let meeting_id = Uuid::new_v4();
    assert_eq!(
        session.start(meeting_id),
        Err(CaptureError::InputDeviceUnavailable)
    );
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::InputDeviceUnavailable,
            recording: None
        }
    );
    assert!(!RecordingLayout::new(&audio, meeting_id).directory.exists());
    assert!(tree(&audio) == before, "every other file is as it was");
    assert!(matches!(session.stop(), Err(CaptureError::InvalidState(_))));
}

/// A start with the id of a meeting whose folder exists is refused before
/// any device opens, and every file in it stays as it was.
#[test]
fn a_start_into_a_meeting_folder_that_exists_is_refused_and_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let audio = directory.path();
    let existing = audio_folder_with_a_meeting(audio);
    let before = tree(audio);
    for backend in [
        Arc::new(SyntheticCaptureBackend::new(tones(&call(), 1.0))) as Arc<dyn CaptureBackend>,
        Arc::new(Failing),
    ] {
        let session = CaptureSession::with_backend(
            configuration(CaptureMode::Call, audio, false),
            backend,
            passthrough(),
            200,
            Arc::new(SystemClock::new()),
        )
        .unwrap();
        let refused =
            CaptureError::RecordingExists(RecordingLayout::new(audio, existing).directory);
        assert_eq!(session.start(existing), Err(refused.clone()));
        assert_eq!(
            session.state(),
            CaptureState::Failed {
                error: refused,
                recording: None
            }
        );
        assert!(tree(audio) == before, "every file is as it was");
    }
}

/// The real writer, which does not say it created the folder.
struct Unclaimed(RecordingWriter);

impl RecordingWriting for Unclaimed {
    fn files(&self) -> RecordingFiles {
        self.0.files()
    }
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        self.0.write(frames)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.0.sync()
    }
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        self.0.finish()
    }
}

/// A failed start removes a folder only when its writer says it made it.
#[test]
fn a_failed_start_keeps_a_folder_its_writer_does_not_claim() {
    let directory = tempfile::tempdir().unwrap();
    let session = CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory.path(), false),
        Arc::new(Failing),
        None,
        200,
        Arc::new(SystemClock::new()),
        Arc::new(|layout, lanes, keep_raw| {
            Ok(
                Box::new(Unclaimed(RecordingWriter::new(layout, lanes, keep_raw)?))
                    as Box<dyn RecordingWriting>,
            )
        }),
    )
    .unwrap();
    let meeting_id = Uuid::new_v4();
    assert!(session.start(meeting_id).is_err());
    assert!(
        RecordingLayout::new(directory.path(), meeting_id)
            .master(AudioFormat::Caf48kFloat32)
            .exists()
    );
}

// Device changes

/// A device change rebuilds the backend in place: the state never leaves
/// `Recording`, the notices say what happened, the master keeps growing on
/// the same files with every frame of both starts and nothing dropped, the
/// echo canceller starts cold again, and no silence is needed when the
/// restart succeeds at once.
#[test]
fn a_device_change_keeps_recording_on_the_same_files() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .change_device_after(1.0)
            .stream_after_restart(restarted_stream()),
    ));
    let log = Arc::new(Mutex::new(Vec::new()));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        Some(Box::new(ResetLoggingCanceller { log: log.clone() })),
        1_000,
        clock.clone(),
    )
    .unwrap();
    let states = session.states();
    let notices = session.notices();
    let meeting_id = Uuid::new_v4();
    session.start(meeting_id).unwrap();
    assert_eq!(session.stream(), Some(CaptureStream::SYNTHETIC));

    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 0.0
        }
    );
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    assert_eq!(
        session.stream(),
        Some(restarted_stream()),
        "the rebuilt backend's stream"
    );
    assert_eq!(backend.starts(), 2);
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);

    let seen = collect_states(&states, until_idle_again());
    assert_eq!(
        kinds(&seen),
        ["idle", "starting", "recording", "stopping", "idle"],
        "the state never left Recording during the change"
    );
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert!(result.statistics.dropped_frames.is_empty());
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(
        backend.frames_delivered(),
        3 * 48_000,
        "one second, then two"
    );
    let master = master_of(&result);
    assert_eq!(master.channels.len(), 2);
    assert_eq!(
        master.frame_count(),
        backend.frames_delivered(),
        "every frame of both starts"
    );
    assert_eq!(result.statistics.duration, 3.0);
    let layout = RecordingLayout::new(directory.path(), meeting_id);
    assert_eq!(
        file_url_path(&result.asset.url).unwrap(),
        layout.master(AudioFormat::Caf48kFloat32),
        "the same files"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["reset", "process", "reset", "process"],
        "cold filter after"
    );
    assert_eq!(clock.pending_sleepers(), 0);
}

/// Keeps every far-end sample it is handed and passes the microphone
/// through.
struct FarEndRecorder {
    far_end: Arc<Mutex<Vec<f32>>>,
}

impl EchoCanceller for FarEndRecorder {
    fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]) {
        self.far_end.lock().unwrap().extend_from_slice(far_end);
        let count = near_end.len().min(out.len());
        out[..count].copy_from_slice(&near_end[..count]);
    }
}

/// A headset in the hands-free profile keeps the Mac's aggregate at
/// 24 kHz: the recording still starts, the master and the sidecars are 48
/// and 16 kHz with the tones at their frequencies and levels, and the far
/// end reaches the canceller delayed by the device latencies in 48 kHz
/// frames.
#[test]
fn a_device_at_24_khz_records_the_usual_files() {
    let directory = tempfile::tempdir().unwrap();
    let hands_free = CaptureStream {
        sample_rate: 24_000.0,
        input_latency_frames: 240,
        output_latency_frames: 4_800,
        ..CaptureStream::SYNTHETIC
    };
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 3.0).stream(hands_free),
    ));
    let far_end = Arc::new(Mutex::new(Vec::new()));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        Some(Box::new(FarEndRecorder {
            far_end: far_end.clone(),
        })),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(session.stream().unwrap().sample_rate, 24_000.0);
    backend.wait_until_finished();
    let result = session.stop().unwrap();

    assert_eq!(backend.frames_delivered(), 3 * 24_000);
    assert!(result.statistics.dropped_frames.is_empty());
    // 72 000 device samples less half a converter window convert to
    // 2 * (72 000 - 32) = 143 936 outputs: 299 whole frames.
    let master = master_of(&result);
    assert_eq!(master.frame_count(), 143_520);
    assert_eq!(result.statistics.duration, 143_520.0 / SAMPLE_RATE);
    assert_eq!(master.sample_rate, SAMPLE_RATE);
    assert_eq!(master.channels.len(), 2);
    for (channel, hertz) in [(0, 440.0), (1, 1_000.0)] {
        let steady = &master.channels[channel][4_800..140_000];
        let measured = frequency(steady, SAMPLE_RATE);
        assert!((measured - hertz).abs() < 2.0, "{measured} Hz");
        let level = level_against_sine(steady, 0.5);
        assert!(level.abs() < 0.1, "{level} dB");
    }
    let mic = sidecar_of(&result, AudioLane::Mic);
    assert_eq!(mic.len(), 143_520 / 3);
    // 240 + 4 800 frames at 24 kHz are 10 080 at 48 kHz: the far end is
    // the system lane that many samples late, zeros before.
    let far_end = far_end.lock().unwrap();
    let system = &master.channels[1];
    assert_eq!(far_end.len(), system.len());
    assert!(far_end[..10_080].iter().all(|s| *s == 0.0));
    assert_eq!(&far_end[10_080..], &system[..system.len() - 10_080]);
}

/// The call starts and the headset enters the hands-free profile while
/// recording: the rebuilt backend comes back at 24 kHz and the recording
/// carries on instead of ending as a lost device.
#[test]
fn a_change_to_24_khz_resumes_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let hands_free = CaptureStream {
        sample_rate: 24_000.0,
        input_latency_frames: 240,
        output_latency_frames: 4_800,
        ..CaptureStream::SYNTHETIC
    };
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .change_device_after(1.0)
            .stream_after_restart(hands_free.clone()),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 0.0
        }
    );
    assert_eq!(session.stream(), Some(hands_free.clone()));
    backend.wait_until_finished();
    let result = session.stop().unwrap();

    assert_eq!(backend.frames_delivered(), 48_000 + 2 * 24_000);
    assert_eq!(result.statistics.device_changes, 1);
    assert!(!result.statistics.ended_on_device_loss);
    assert!(result.statistics.dropped_frames.is_empty());
    // 100 frames at 48 kHz, then 48 000 samples at 24 kHz less half a
    // window: 2 * (48 000 - 32) = 95 936 outputs, 199 whole frames.
    let master = master_of(&result);
    assert_eq!(master.frame_count(), (100 + 199) * 480);
    let after = &master.channels[0][52_800..140_000];
    let measured = frequency(after, SAMPLE_RATE);
    assert!(
        (measured - 440.0).abs() < 2.0,
        "{measured} Hz after the change"
    );
}

/// The contiguity claim: a gap longer than the two seconds the sink's rings
/// hold is written in full as silence through the relay, so the master runs
/// to wall time with nothing truncated into `dropped_frames`. Three restarts
/// fail, the clock advances 0.25, 0.5 and then 2.5 s, the fourth succeeds,
/// and 3.25 s of zeros sit between the old device's last frame and the new
/// device's first.
#[test]
fn a_gap_longer_than_the_ring_is_written_in_full() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .change_device_after(1.0)
            .restarts_that_fail(3),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        clock.clone(),
    )
    .unwrap();
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 2);
    assert!(clock.wait_for_sleepers(1), "the third backoff");
    clock.advance(Duration::from_millis(2_500));
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 4,
            gap_seconds: 3.25
        }
    );
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    backend.wait_until_finished();
    let result = session.stop().unwrap();

    let seen = collect_states(&states, until_idle_again());
    assert_eq!(
        kinds(&seen),
        ["idle", "starting", "recording", "stopping", "idle"]
    );
    assert_eq!(result.statistics.gap_seconds, 3.25);
    assert_eq!(result.statistics.device_changes, 1);
    assert!(result.statistics.dropped_frames.is_empty());
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(backend.starts(), 5);
    let old_frames = 48_000;
    let gap_frames = (3.25 * 48_000.0) as usize;
    assert_eq!(backend.frames_delivered(), 3 * 48_000);
    let master = master_of(&result);
    assert_eq!(
        master.frame_count(),
        backend.frames_delivered() + gap_frames
    );
    assert_eq!(result.statistics.duration, 6.25);
    for channel in &master.channels {
        assert!(
            channel[old_frames - 480..old_frames]
                .iter()
                .any(|s| *s != 0.0),
            "the old device's last frame precedes the gap"
        );
        assert!(
            channel[old_frames..old_frames + gap_frames]
                .iter()
                .all(|s| *s == 0.0),
            "the gap is silence"
        );
        assert!(
            channel[old_frames + gap_frames..old_frames + gap_frames + 480]
                .iter()
                .any(|s| *s != 0.0),
            "the new device's first frame follows it"
        );
    }
    let sidecar = sidecar_of(&result, AudioLane::Mic);
    assert_eq!(
        sidecar.len(),
        master.frame_count() / 3,
        "the sidecars carry the gap too"
    );
}

/// The shape a Bluetooth headset produces (out of the profile and back):
/// two changes, two rebuilds, four notices in order, one master.
#[test]
fn two_device_changes_rebuild_twice() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0).change_device_after(0.5).changes(2),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        clock,
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    let seen: Vec<CaptureNotice> = (0..4)
        .filter_map(|_| notices.recv_timeout(RECV).ok())
        .collect();
    assert_eq!(
        seen,
        [
            CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged),
            CaptureNotice::DeviceResumed {
                attempt: 1,
                gap_seconds: 0.0
            },
            CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged),
            CaptureNotice::DeviceResumed {
                attempt: 1,
                gap_seconds: 0.0
            },
        ]
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 2);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert!(result.statistics.dropped_frames.is_empty());
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(backend.starts(), 3);
    assert_eq!(backend.frames_delivered(), 24_000 + 24_000 + 96_000);
    assert_eq!(master_of(&result).frame_count(), backend.frames_delivered());
}

/// Four failed restarts end the recording in `DeviceLost` after the summed
/// backoff on the manual clock, with what was recorded before the change
/// and nothing rebuilt.
#[test]
fn a_restart_that_keeps_failing_ends_in_device_lost() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(0.5)
            .restarts_that_fail(4),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        clock.clone(),
    )
    .unwrap();
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    // Three sleeps separate the four attempts; none before the first.
    advance_through_sleeps(&clock, 3);
    assert_eq!(clock.now(), Duration::from_millis(1_750));
    let seen = collect_states(&states, until_failed);
    assert_eq!(
        seen.last().unwrap().failure(),
        Some(&CaptureError::DeviceLost)
    );
    let result = session.stop().unwrap();
    assert!(result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 0);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert_eq!(result.statistics.duration, 0.5);
    assert_eq!(master_of(&result).frame_count(), 24_000);
    assert_eq!(backend.starts(), 5, "one start and four failed restarts");
    assert_eq!(clock.pending_sleepers(), 0);
    settle();
    let entries: Vec<CaptureNotice> = notices.try_iter().collect();
    assert_eq!(
        entries,
        [CaptureNotice::DeviceChanged(
            DeviceChangeReason::DefaultInputChanged
        )],
        "no resumed notice"
    );
}

/// A backend whose `stop()` reports a change on the sink it was given, the
/// way a HAL listener can fire while the session tears down.
struct ReportingOnStop {
    inner: Arc<SyntheticCaptureBackend>,
    sink: Mutex<Option<Arc<LaneFrameSink>>>,
}

impl CaptureBackend for ReportingOnStop {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        *self.sink.lock().unwrap() = Some(sink.clone());
        self.inner.start(lanes, uid, sink)
    }
    fn stop(&self) {
        self.inner.stop();
        if let Some(sink) = self.sink.lock().unwrap().clone() {
            sink.report_device_change(DeviceChangeReason::OutputDeviceGone);
        }
    }
}

/// A report before `start` and one from inside `stop()` produce no notice
/// and leave the state as it was.
#[test]
fn a_device_change_while_idle_or_after_stop_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let inner = Arc::new(SyntheticCaptureBackend::new(tones(
        &[AudioLane::Mixed],
        0.2,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        Arc::new(ReportingOnStop {
            inner: inner.clone(),
            sink: Mutex::new(None),
        }),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let notices = session.notices();
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    assert_eq!(session.state(), CaptureState::Idle);

    session.start(Uuid::new_v4()).unwrap();
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    settle();
    assert_eq!(session.state(), CaptureState::Idle);
    assert!(notices.try_iter().next().is_none());
    assert_eq!(result.statistics.device_changes, 0);
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(inner.starts(), 1, "nothing was restarted");
}

/// A report the first recording's backend sends once the second recording
/// started, as one left in its handler past the first's stop: the second
/// recording is not rebuilt.
#[test]
fn a_report_from_an_earlier_recordings_backend_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(HandsOverTheSink::new(CaptureStream::SYNTHETIC));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        200,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    let first = backend.sink.lock().unwrap().clone().unwrap();
    session.stop().unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    first.report_device_change(DeviceChangeReason::OutputDeviceGone);
    settle();
    assert!(
        matches!(session.state(), CaptureState::Recording { .. }),
        "the second recording runs on"
    );
    assert!(notices.try_iter().next().is_none(), "no rebuild began");
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 0);
}

/// The writer fails while a rebuild is under way (on the first frame of gap
/// silence, the 51st frame written): the session ends `Failed(WriterFailed)`
/// with what was written, not `DeviceLost`, and the rebuild does not
/// resurrect it.
#[test]
fn a_writer_failure_during_a_rebuild_ends_writer_failed() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 2.0)
            .change_device_after(0.5)
            .restarts_that_fail(1),
    ));
    let session = faulty_session(
        directory.path(),
        backend.clone(),
        Some(50),
        false,
        clock.clone(),
    );
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    // One failed restart, 250 ms on the clock, then 25 frames of silence.
    advance_through_sleeps(&clock, 1);
    let seen = collect_states(&states, until_failed);
    let (detail, recording) = match seen.last() {
        Some(CaptureState::Failed {
            error: CaptureError::WriterFailed(detail),
            recording,
        }) => (detail.clone(), recording.clone()),
        other => panic!("expected Failed(WriterFailed), got {other:?}"),
    };
    assert!(detail.contains("DiskFull"));
    let result = session.stop().unwrap();
    assert_eq!(recording.as_deref(), Some(&result));
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.duration, 0.5);
    assert_eq!(master_of(&result).frame_count(), 50 * 480);
    settle();
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::WriterFailed(detail),
            recording: Some(Box::new(result))
        }
    );
    assert!(clock.wait_for_sleepers(0));
}

/// `stop()` while the rebuild waits out a backoff abandons it: the
/// recording is finalised once, ends `Idle`, the cancelled sleep is gone,
/// and nothing the clock does afterwards changes the outcome.
#[test]
fn stop_during_a_rebuild_finalises_once() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(0.5)
            .restarts_that_fail(4),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        clock.clone(),
    )
    .unwrap();
    let states = session.states();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(
        clock.wait_for_sleepers(1),
        "the rebuild is waiting out the first backoff"
    );

    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert!(
        clock.wait_for_sleepers(0),
        "the abandoned rebuild's sleep was cancelled"
    );
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 0);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert_eq!(result.statistics.duration, 0.5);
    assert_eq!(master_of(&result).frame_count(), 24_000);
    assert_eq!(backend.starts(), 2, "one start, one failed restart");

    clock.advance(Duration::from_secs(10));
    settle();
    assert_eq!(session.state(), CaptureState::Idle);
    assert_eq!(backend.starts(), 2, "nothing after the stop");
    assert!(matches!(session.stop(), Err(CaptureError::InvalidState(_))));
    let seen = collect_states(&states, until_idle_again());
    assert_eq!(
        kinds(&seen),
        ["idle", "starting", "recording", "stopping", "idle"]
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

/// The live backend's shape during a device transition: a producer of 0.25
/// on every lane every 10 ms that runs on until `stop()` has torn it down.
/// `stop()` call number `gated_call` (counted from 1) reports itself on
/// `at_gate` and then waits for the test to open the gate, with the
/// producer still running if one is; it takes the producer first, so a
/// second `stop()` meanwhile returns at once, as a backend whose teardown
/// is already under way does. `change_after` reports one device change
/// after that many callbacks of the first start; with `restarts_fail`
/// every later start fails as an absent device does.
struct GatedStop {
    gated_call: usize,
    change_after: Option<usize>,
    restarts_fail: bool,
    stops: AtomicUsize,
    starts: AtomicUsize,
    running: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>>,
    alive: Arc<AtomicUsize>,
    delivered: Arc<AtomicUsize>,
    at_gate: Mutex<Sender<()>>,
    gate: Mutex<Receiver<()>>,
}

impl GatedStop {
    /// The backend, the receiver `at_gate` reports on and the gate's
    /// sender.
    fn new(
        gated_call: usize,
        change_after: Option<usize>,
        restarts_fail: bool,
    ) -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (at_gate, reached) = channel();
        let (open, gate) = channel();
        let backend = Self {
            gated_call,
            change_after,
            restarts_fail,
            stops: AtomicUsize::new(0),
            starts: AtomicUsize::new(0),
            running: Mutex::new(None),
            alive: Arc::new(AtomicUsize::new(0)),
            delivered: Arc::new(AtomicUsize::new(0)),
            at_gate: Mutex::new(at_gate),
            gate: Mutex::new(gate),
        };
        (Arc::new(backend), reached, open)
    }

    fn delivered(&self) -> usize {
        self.delivered.load(Ordering::SeqCst)
    }
}

impl CaptureBackend for GatedStop {
    fn start(
        &self,
        lanes: &[AudioLane],
        _uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let call = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        let first = call == 1;
        if !first && self.restarts_fail {
            return Err(CaptureError::InputDeviceUnavailable);
        }
        let change_after = if first { self.change_after } else { None };
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let alive = Arc::clone(&self.alive);
        let delivered = Arc::clone(&self.delivered);
        let lane_count = lanes.len();
        alive.fetch_add(1, Ordering::SeqCst);
        let producer = std::thread::spawn(move || {
            let buffer = vec![0.25f32; 480];
            let mut count = 0;
            while !stopped.load(Ordering::Acquire) {
                if sink.begin_callback(480) {
                    for lane in 0..lane_count {
                        sink.write_slice(lane, &buffer);
                    }
                    sink.end_callback();
                    delivered.fetch_add(480, Ordering::SeqCst);
                }
                count += 1;
                if change_after == Some(count) {
                    sink.report_device_change(DeviceChangeReason::DefaultInputChanged);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            alive.fetch_sub(1, Ordering::SeqCst);
        });
        *self.running.lock().unwrap() = Some((stop, producer));
        Ok(CaptureStream::SYNTHETIC)
    }

    fn stop(&self) {
        let call = self.stops.fetch_add(1, Ordering::SeqCst) + 1;
        let running = self.running.lock().unwrap().take();
        if call == self.gated_call {
            self.at_gate.lock().unwrap().send(()).unwrap();
            self.gate.lock().unwrap().recv().unwrap();
        }
        if let Some((stop, producer)) = running {
            stop.store(true, Ordering::Release);
            producer.join().unwrap();
        }
    }
}

/// Calls `stop()` on its own thread while another thread (a finalise, or a
/// rebuild's teardown) waits at `backend`'s gate, and opens the gate once
/// the call has returned (a `stop()` that does not wait returns at once) or
/// once 200 ms have passed and the state reads `Stopping`. A rebuild's
/// teardown runs while `Recording`, so the gate stays shut until the
/// `stop()` has set `Stopping`, which every later rebuild step checks,
/// however late its thread runs. A finalise holds `Stopping` throughout,
/// and a `stop()` that arrives after it ends gets the same answer. After
/// `RECV` the gate opens regardless and the caller's assertions fail.
/// Returns what `stop()` returned and the producers alive at that moment.
fn stop_at_the_gate(
    session: &CaptureSession,
    backend: &GatedStop,
    open: &Sender<()>,
) -> (Result<CaptureResult, CaptureError>, usize) {
    std::thread::scope(|scope| {
        let stopper = scope.spawn(|| {
            let result = session.stop();
            (result, backend.alive.load(Ordering::SeqCst))
        });
        let started = Instant::now();
        while !stopper.is_finished()
            && started.elapsed() < RECV
            && (started.elapsed() < Duration::from_millis(200)
                || session.state() != CaptureState::Stopping)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        open.send(()).unwrap();
        stopper.join().unwrap()
    })
}

/// `stop()` while the rebuild is inside a slow backend teardown waits for
/// that teardown instead of clearing the rings under a running producer and
/// the old processing thread: when `stop()` returns nothing produces any
/// more, every frame delivered is in the master, nothing was dropped, the
/// abandoned rebuild started nothing, and the session records again.
#[test]
fn stop_during_a_rebuilds_teardown_waits_for_it() {
    let directory = tempfile::tempdir().unwrap();
    let (backend, at_gate, open) = GatedStop::new(1, Some(10), false);
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        Arc::new(ManualClock::new()),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    at_gate
        .recv_timeout(RECV)
        .expect("the rebuild is tearing the backend down");

    let (result, alive) = stop_at_the_gate(&session, &backend, &open);
    let result = result.unwrap();
    assert_eq!(alive, 0, "the producer ran on after stop() returned");
    assert_eq!(session.state(), CaptureState::Idle);
    assert_eq!(backend.starts.load(Ordering::SeqCst), 1, "no restart");
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(result.statistics.device_changes, 0);
    assert_eq!(result.statistics.gap_seconds, 0.0);
    assert!(
        !result.statistics.system_lane_silent,
        "the old processing thread's peak counts"
    );
    let delivered = backend.delivered();
    let master = master_of(&result);
    assert_eq!(master.frame_count(), delivered);
    assert_eq!(result.statistics.duration, delivered as f64 / SAMPLE_RATE);

    session.start(Uuid::new_v4()).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let again = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert!(again.statistics.dropped_frames.is_empty());
}

/// The disk fills while recording and a user's `stop()` arrives while that
/// failure is still finalising. It waits for the finalise, as Swift's actor
/// ordered the two, and returns the recording the failure carries: no
/// error, and no producer running when it returns. Whether this `stop()`
/// waited or arrived after the finalise is not observable here; the unit
/// tests in `src/capture/session.rs` force the wait's window, and a
/// `start()` that overtakes it, deterministically.
#[test]
fn stop_during_a_writer_failures_finalise_returns_its_recording() {
    let directory = tempfile::tempdir().unwrap();
    let (backend, at_gate, open) = GatedStop::new(1, None, false);
    let session = faulty_session(
        directory.path(),
        backend.clone(),
        Some(5),
        false,
        Arc::new(SystemClock::new()),
    );
    session.start(Uuid::new_v4()).unwrap();
    at_gate
        .recv_timeout(RECV)
        .expect("the writer failure's finalise is stopping the backend");
    assert_eq!(session.state(), CaptureState::Stopping);

    let (result, alive) = stop_at_the_gate(&session, &backend, &open);
    let result = result.expect("stop() returns the finalised recording");
    assert_eq!(alive, 0, "a producer still ran when stop() returned");
    assert_eq!(master_of(&result).frame_count(), 5 * 480);
    match session.state() {
        CaptureState::Failed {
            error: CaptureError::WriterFailed(detail),
            recording,
        } => {
            assert!(detail.contains("DiskFull"));
            assert_eq!(recording.as_deref(), Some(&result));
        }
        other => panic!("expected Failed(WriterFailed), got {other:?}"),
    }
}

/// Every restart fails and a `stop()` arrives while the device loss is
/// finalising: it waits and returns the recording `Failed(DeviceLost)`
/// carries, every delivered frame in it.
#[test]
fn stop_during_the_device_loss_finalise_returns_its_recording() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let (backend, at_gate, open) = GatedStop::new(2, Some(10), true);
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        200,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 3);
    at_gate
        .recv_timeout(RECV)
        .expect("the device loss's finalise is stopping the backend");
    assert_eq!(session.state(), CaptureState::Stopping);

    let (result, alive) = stop_at_the_gate(&session, &backend, &open);
    let result = result.expect("stop() returns the finalised recording");
    assert_eq!(alive, 0, "a producer still ran when stop() returned");
    assert!(result.statistics.ended_on_device_loss);
    assert!(!result.statistics.system_lane_silent);
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(master_of(&result).frame_count(), backend.delivered());
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::DeviceLost,
            recording: Some(Box::new(result))
        }
    );
}

/// Runs `f` on its own thread and fails the test when it has not returned
/// within `RECV`, rather than hanging the test binary.
fn within_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = channel();
    std::thread::spawn(move || {
        let _ = done.send(f());
    });
    match finished.recv_timeout(RECV) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => panic!("{what} did not return within {RECV:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

/// The synthetic backend, except that the `stop()` calls numbered in
/// `panicking` (counted from 1) stop the producer and then panic, as a
/// backend with a bug would.
struct PanicsOnStop {
    inner: SyntheticCaptureBackend,
    panicking: Vec<usize>,
    stops: AtomicUsize,
    panicked: AtomicUsize,
}

impl CaptureBackend for PanicsOnStop {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        self.inner.start(lanes, uid, sink)
    }

    fn stop(&self) {
        let call = self.stops.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.stop();
        if self.panicking.contains(&call) {
            self.panicked.fetch_add(1, Ordering::SeqCst);
            panic!("backend stop {call} panics on purpose");
        }
    }
}

/// A finalise that panics leaves `Stopping` for `Failed` with no recording
/// rather than holding it forever: a later `stop()` returns at once, and the
/// session records again. Both finalisers are covered: a writer failure's,
/// on its own thread, and the caller's `stop()`, which passes the panic on.
#[test]
fn a_panicking_finalise_ends_failed_and_the_session_starts_again() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(PanicsOnStop {
        inner: SyntheticCaptureBackend::new(tones(&[AudioLane::Mixed], 0.5)),
        panicking: vec![1, 3],
        stops: AtomicUsize::new(0),
        panicked: AtomicUsize::new(0),
    });
    let session = Arc::new(first_writer_fails(
        directory.path(),
        backend.clone(),
        CaptureMode::InPerson,
    ));
    let no_recording =
        || CaptureError::InvalidState("stop after a failure that left no recording".into());

    // The writer failure's finalise panics on its own thread.
    session.start(Uuid::new_v4()).unwrap();
    let deadline = Instant::now() + RECV;
    while backend.panicked.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the finalise never stopped the backend"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopping = Arc::clone(&session);
    let stopped = within_deadline("stop() after a panicked finalise", move || stopping.stop());
    assert_eq!(stopped.unwrap_err(), no_recording());
    assert!(
        matches!(
            session.state(),
            CaptureState::Failed {
                error: CaptureError::WriterFailed(_),
                recording: None
            }
        ),
        "{:?}",
        session.state()
    );

    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    assert_eq!(master_of(&session.stop().unwrap()).frame_count(), 24_000);

    // The caller's own `stop()` panics; the panic reaches the caller.
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.stop()));
    assert!(unwound.is_err(), "the backend's panic reaches the caller");
    assert_eq!(
        session.state(),
        CaptureState::Failed {
            error: CaptureError::BackendFailed("the teardown panicked".into()),
            recording: None
        }
    );
    let stopping = Arc::clone(&session);
    let stopped = within_deadline("stop() after a panicked stop()", move || stopping.stop());
    assert_eq!(stopped.unwrap_err(), no_recording());

    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    assert_eq!(master_of(&session.stop().unwrap()).frame_count(), 24_000);
    assert_eq!(session.state(), CaptureState::Idle);
}

/// A session over `backend` whose first recording's writer fails after
/// five frames, as a full disk does; every later one writes normally.
fn first_writer_fails(
    directory: &Path,
    backend: Arc<dyn CaptureBackend>,
    mode: CaptureMode,
) -> CaptureSession {
    let first = Arc::new(AtomicBool::new(true));
    CaptureSession::with_writer_factory(
        configuration(mode, directory, false),
        backend,
        None,
        1_000,
        Arc::new(SystemClock::new()),
        Arc::new(move |layout, lanes, keep_raw| {
            let writer = RecordingWriter::new(layout, lanes, keep_raw)?;
            Ok(if first.swap(false, Ordering::SeqCst) {
                Box::new(FaultyWriter {
                    inner: writer,
                    fail_after_frames: Some(5),
                    fail_finish: false,
                    frames: 0,
                }) as Box<dyn RecordingWriting>
            } else {
                Box::new(writer)
            })
        }),
    )
    .unwrap()
}

/// Fails every write once `full` is set, as a disk that fills does.
struct FullFrom(RecordingWriter, Arc<AtomicBool>);

impl RecordingWriting for FullFrom {
    fn files(&self) -> RecordingFiles {
        self.0.files()
    }
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        if self.1.load(Ordering::SeqCst) {
            return Err(CaptureError::WriterFailed("DiskFull".into()));
        }
        self.0.write(frames)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        if self.1.load(Ordering::SeqCst) {
            return Err(std::io::ErrorKind::StorageFull.into());
        }
        self.0.sync()
    }
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        self.0.finish()
    }
}

/// The disk fills during `stop()`'s own drain: the backend delivers on
/// while its teardown runs, and those frames fail to write. `stop()` still
/// returns the recording, and the state carries the failure instead of
/// reading `Idle` over a master shorter than what was delivered.
#[test]
fn a_write_failing_during_stops_drain_ends_failed_with_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let (backend, at_gate, open) = GatedStop::new(1, None, false);
    let full = Arc::new(AtomicBool::new(false));
    let disk = Arc::clone(&full);
    let session = CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        1_000,
        Arc::new(SystemClock::new()),
        Arc::new(move |layout, lanes, keep_raw| {
            Ok(Box::new(FullFrom(
                RecordingWriter::new(layout, lanes, keep_raw)?,
                Arc::clone(&disk),
            )) as Box<dyn RecordingWriting>)
        }),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    let result = std::thread::scope(|scope| {
        let stopper = scope.spawn(|| session.stop());
        at_gate
            .recv_timeout(RECV)
            .expect("stop() is tearing the backend down");
        full.store(true, Ordering::SeqCst);
        // Asserted once the gate is open: a panic while `stopper` waits at
        // the gate would leave the scope joining it forever.
        let before = backend.delivered();
        let deadline = Instant::now() + RECV;
        while backend.delivered() == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let delivered_on = backend.delivered() > before;
        open.send(()).unwrap();
        let returned = stopper.join().unwrap();
        assert!(
            delivered_on,
            "the backend delivered nothing during the teardown within {RECV:?}"
        );
        returned
    })
    .expect("stop() returns the recording");
    assert!(master_of(&result).frame_count() < backend.delivered());
    assert!(
        matches!(&result.failure, Some(CaptureError::WriterFailed(detail)) if detail.contains("DiskFull")),
        "the result carries the failure: {:?}",
        result.failure
    );
    match session.state() {
        CaptureState::Failed {
            error: CaptureError::WriterFailed(detail),
            recording,
        } => {
            assert!(detail.contains("DiskFull"));
            assert_eq!(recording.as_deref(), Some(&result));
        }
        other => panic!("expected Failed(WriterFailed), got {other:?}"),
    }
}

/// A gap wider than the relay waits for the writer in 5 ms steps on the
/// clock, and every frame of silence still arrives: 1.75 s of gap through
/// a one-second relay (production's holds 20 s, more than `MAXIMUM_GAP`,
/// so there only a stalled writer makes a gap wait). The old
/// device's half second fits the relay however late the writer runs; only
/// a writer stalled for longer than the relay could refuse the new
/// device's frames, and those are counted as dropped.
#[test]
fn a_gap_wider_than_the_relay_waits_for_the_writer_and_writes_all_its_silence() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 1.0)
            .change_device_after(0.5)
            .restarts_that_fail(3),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        100,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    advance_through_sleeps(&clock, 3);
    // The rebuilt backend delivers its second at machine speed into the
    // rings while the gap waits on the clock; the writer drains the relay
    // in real time, so the gap's 5 ms waits are driven as they appear.
    let mut seen = vec![notices.recv_timeout(RECV).unwrap()];
    let deadline = Instant::now() + RECV;
    while seen.len() < 2 {
        assert!(Instant::now() < deadline, "no resume within {RECV:?}");
        if let Ok(notice) = notices.recv_timeout(Duration::from_millis(2)) {
            seen.push(notice);
        } else if clock.pending_sleepers() == 1 {
            clock.advance(Duration::from_millis(5));
        }
    }
    assert_eq!(
        seen,
        [
            CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged),
            CaptureNotice::DeviceResumed {
                attempt: 4,
                gap_seconds: 1.75
            },
        ]
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, 1.75);
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(backend.frames_delivered(), 24_000 + 48_000);
    let master = master_of(&result);
    let dropped = |lane| {
        result
            .statistics
            .dropped_frames
            .get(&lane)
            .copied()
            .unwrap_or(0)
    };
    let refused = dropped(AudioLane::Mic);
    assert!(
        refused < 100,
        "the writer drained the new device: {refused}"
    );
    assert_eq!(dropped(AudioLane::System), refused);
    assert_eq!(
        master.frame_count() + 480 * refused,
        backend.frames_delivered() + 84_000
    );
    for channel in &master.channels {
        assert!(channel[24_000..108_000].iter().all(|s| *s == 0.0));
        assert!(
            channel[108_000..].iter().any(|s| *s != 0.0),
            "the new device's audio follows the gap"
        );
    }
    assert_eq!(
        result.statistics.duration,
        master.frame_count() as f64 / SAMPLE_RATE
    );
    assert!(clock.wait_for_sleepers(0));
}

/// `stop()` while the gap waits for room in a 30-frame relay: the silence
/// already written stays in the master and is reported in `gap_seconds`,
/// though the rebuild never resumed, so `device_changes` stays 0, and what
/// the restarted backend delivered meanwhile counts as dropped. The relay
/// may still hold the old device's last audio when the gap starts, so the
/// silence written can be anything from none to a whole relay; a writer
/// that stalls while the old device delivers fills the relay, and the
/// frames it refuses are dropped too (see
/// `a_stalled_writer_refuses_old_audio_and_the_gap_waits_behind_it`).
#[test]
fn stop_while_the_gap_waits_for_the_relay_reports_the_silence_written() {
    stop_while_the_gap_waits_for_the_relay(SAMPLE_RATE);
}

/// The same with the restarted device at 24 kHz: what it left in the rings
/// is counted in 48 kHz frames, at its own rate and not the stream's the
/// stop replaced.
#[test]
fn stop_while_the_gap_waits_counts_a_restarted_24_khz_device_in_48_khz_frames() {
    stop_while_the_gap_waits_for_the_relay(24_000.0);
}

/// The two tests above, with the restarted backend at `restarted_rate`.
fn stop_while_the_gap_waits_for_the_relay(restarted_rate: f64) {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 1.0)
            .change_device_after(0.5)
            .restarts_that_fail(3)
            .stream_after_restart(CaptureStream {
                sample_rate: restarted_rate,
                ..CaptureStream::SYNTHETIC
            })
            .real_time(true),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        30,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 3);
    // The fourth start succeeded; 1.75 s of gap does not fit the relay.
    assert!(
        clock.wait_for_sleepers(1),
        "the gap waits for the relay on the clock"
    );
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    let master = master_of(&result);
    // The master is the old device's audio the relay took, then the
    // silence `gap_seconds` reports.
    let silence = (result.statistics.gap_seconds * SAMPLE_RATE).round() as usize;
    let audio = master
        .frame_count()
        .checked_sub(silence)
        .expect("the master holds the silence reported");
    assert!(
        audio <= 24_000 && audio.is_multiple_of(480),
        "old audio: {audio}"
    );
    assert!(master.channels[0][audio..].iter().all(|s| *s == 0.0));
    assert_eq!(result.statistics.device_changes, 0);
    // The old device's frames the relay refused, and what the restarted
    // backend delivered into the rings with no processing thread to drain
    // it: dropped, and counted.
    let refused = (24_000 - audio) / 480;
    // The restarted device's samples, in 48 kHz frames.
    let undrained =
        (backend.frames_delivered() - 24_000) * (SAMPLE_RATE / restarted_rate) as usize / 480;
    assert_eq!(
        result
            .statistics
            .dropped_frames
            .get(&AudioLane::Mixed)
            .copied()
            .unwrap_or(0),
        undrained + refused
    );
    assert_eq!(
        result.statistics.duration,
        master.frame_count() as f64 / SAMPLE_RATE
    );
    assert!(clock.wait_for_sleepers(0));
}

/// Where [`HeldWrites`] holds the writer.
#[derive(Debug, PartialEq)]
enum Held {
    FirstWrite,
    FirstSilence,
}

/// The production writer, held on its first write and on the first frame
/// of silence after it until the test lets each go, as a stalled disk
/// holds it.
struct HeldWrites {
    inner: RecordingWriter,
    next: Option<Held>,
    at_hold: Sender<Held>,
    release: Receiver<()>,
}

impl RecordingWriting for HeldWrites {
    fn files(&self) -> RecordingFiles {
        self.inner.files()
    }
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        let silent = frames
            .lanes
            .iter()
            .all(|lane| lane.iter().all(|s| *s == 0.0));
        let hold = match self.next {
            Some(Held::FirstWrite) => self.next.replace(Held::FirstSilence),
            Some(Held::FirstSilence) if silent => self.next.take(),
            _ => None,
        };
        if let Some(hold) = hold {
            let _ = self.at_hold.send(hold);
            // Bounded, so a failed test cannot hang its session's drop.
            let _ = self.release.recv_timeout(RECV);
        }
        self.inner.write(frames)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.sync()
    }
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        self.inner.finish()
    }
}

/// A writer stalled while the old device delivers: the 30-frame relay
/// (34 frames once rounded) fills, and the old device's last frames are
/// refused and counted as dropped. The gap then starts behind a full relay
/// and waits on the clock; once the writer reaches its first frame of
/// silence and stalls again, the gap fills the relay with silence and
/// waits again, where `stop()` finds it. The master holds what the relay
/// took: the old audio less what it refused, then 35 frames of silence.
#[test]
fn a_stalled_writer_refuses_old_audio_and_the_gap_waits_behind_it() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 1.0)
            .change_device_after(0.5)
            .restarts_that_fail(3)
            .real_time(true),
    ));
    let (at_hold, holds) = channel();
    let (release, released) = channel();
    let parts = Mutex::new(Some((at_hold, released)));
    let session = CaptureSession::with_writer_factory(
        configuration(CaptureMode::InPerson, directory.path(), false),
        backend.clone(),
        None,
        30,
        clock.clone(),
        Arc::new(move |layout, lanes, keep_raw| {
            let (at_hold, release) = parts.lock().unwrap().take().expect("one writer");
            Ok(Box::new(HeldWrites {
                inner: RecordingWriter::new(layout, lanes, keep_raw)?,
                next: Some(Held::FirstWrite),
                at_hold,
                release,
            }) as Box<dyn RecordingWriting>)
        }),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(holds.recv_timeout(RECV).unwrap(), Held::FirstWrite);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 3);
    assert!(
        clock.wait_for_sleepers(1),
        "the gap waits behind the old device's audio"
    );
    release.send(()).unwrap();
    // The writer drains the old audio while the gap's waits are stepped,
    // until it holds the first frame of silence.
    let deadline = Instant::now() + RECV;
    loop {
        assert!(
            Instant::now() < deadline,
            "no frame of silence within {RECV:?}"
        );
        match holds.recv_timeout(Duration::from_millis(2)) {
            Ok(held) => {
                assert_eq!(held, Held::FirstSilence);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                if clock.pending_sleepers() == 1 {
                    clock.advance(Duration::from_millis(5));
                }
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the writer is gone"),
        }
    }
    // One more step: the gap fills the relay behind the held frame and
    // waits again.
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_millis(5));
    assert!(clock.wait_for_sleepers(1), "the relay is full of silence");
    release.send(()).unwrap();
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert_eq!(result.statistics.device_changes, 0);

    let master = master_of(&result);
    let silence = (result.statistics.gap_seconds * SAMPLE_RATE).round() as usize;
    assert_eq!(silence, 35 * 480, "the held frame and a full relay");
    let audio = master.frame_count() - silence;
    assert!(master.channels[0][audio..].iter().all(|s| *s == 0.0));
    // Normally the held frame and a full relay: 35 frames, 15 refused. A
    // writer thread that first runs after the whole half second takes 34.
    let refused = (24_000 - audio) / 480;
    assert!(
        audio.is_multiple_of(480) && (15..=16).contains(&refused),
        "old audio: {audio}"
    );
    assert_eq!(master.frame_count() + 480 * refused, 24_000 + silence);
    let undrained = (backend.frames_delivered() - 24_000) / 480;
    assert_eq!(
        result
            .statistics
            .dropped_frames
            .get(&AudioLane::Mixed)
            .copied()
            .unwrap_or(0),
        undrained + refused
    );
    assert!(clock.wait_for_sleepers(0));
}

/// `MAXIMUM_GAP`: an outage of 30 s on the clock fills 10 s of silence, and
/// `gap_seconds` reports the capped value the master actually holds.
#[test]
fn a_gap_is_capped_at_ten_seconds() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .change_device_after(1.0)
            .restarts_that_fail(1),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        2_000,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_secs(30));
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 2,
            gap_seconds: 10.0
        }
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, 10.0);
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(
        master_of(&result).frame_count(),
        backend.frames_delivered() + 480_000
    );
    assert_eq!(result.statistics.duration, 13.0);
}

/// A change reported while a rebuild is under way is not lost: `resume`
/// starts the next rebuild from it, so the notices come in pairs and the
/// second rebuild runs at once.
#[test]
fn a_change_reported_during_a_rebuild_starts_the_next_one() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .change_device_after(0.5)
            .restarts_that_fail(1),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        clock.clone(),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(
        clock.wait_for_sleepers(1),
        "the first restart failed; the rebuild waits"
    );
    session.device_changed(DeviceChangeReason::OutputDeviceGone);
    clock.advance(Duration::from_millis(250));
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 2,
            gap_seconds: 0.25
        }
    );
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::OutputDeviceGone),
        "kept, not dropped"
    );
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 0.0
        }
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 2);
    assert_eq!(result.statistics.gap_seconds, 0.25);
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(
        backend.starts(),
        4,
        "start, failed restart, restart, restart"
    );
    // The second rebuild stopped its backend mid-callback, so only the
    // sub-frame residue is missing from the master.
    let frames = master_of(&result).frame_count();
    let expected = backend.frames_delivered() + 12_000;
    assert!(
        frames <= expected && frames > expected - 480,
        "{frames} of {expected}"
    );
    assert!(clock.wait_for_sleepers(0));
}

// The stall watchdog, the retry of a graph that does not run, and the ask
// for a chosen microphone the session replaced (Rust only)

/// An in-person session over `backend` on `clock`, with 30 s of relay, so
/// a gap of `MAXIMUM_GAP` never waits for the writer on the manual clock.
fn in_person_session(
    directory: &Path,
    backend: Arc<dyn CaptureBackend>,
    clock: Arc<dyn Clock>,
) -> CaptureSession {
    CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory, false),
        backend,
        None,
        3_000,
        clock,
    )
    .unwrap()
}

/// One tone on the room lane for `seconds`, stalling `stall_after` in.
fn stalling(seconds: f64, stall_after: f64) -> SyntheticOptions {
    tones(&[AudioLane::Mixed], seconds).stall_after(stall_after)
}

/// Once the watch thread sleeps, moves `clock` by `by`.
fn advance_watch(clock: &ManualClock, by: Duration) {
    assert!(clock.wait_for_sleepers(1), "the watch thread sleeps");
    clock.advance(by);
}

/// As [`advance_watch`], once `delivered` moved twice after the watch
/// thread's last sample, so that sample's successor sees a stream that
/// delivers. Twice: the count moves after its callback, so the first move
/// may belong to a callback the sink counted before a rebuild's resume
/// read it, which the watch thread would not see move.
fn advance_while_delivering(clock: &ManualClock, delivered: impl Fn() -> usize, by: Duration) {
    assert!(clock.wait_for_sleepers(1), "the watch thread sleeps");
    let deadline = Instant::now() + RECV;
    for _ in 0..2 {
        let before = delivered();
        while delivered() == before {
            assert!(Instant::now() < deadline, "the backend delivers");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    clock.advance(by);
}

/// Asserts that no notice came, once every thread had a chance to run.
fn assert_no_notice(notices: &Receiver<CaptureNotice>, context: &str) {
    settle();
    assert_eq!(notices.try_recv().ok(), None, "{context}");
}

/// Drives the watch thread over a backend that stalled after delivering:
/// it sees the frames at the first sample, then none for exactly
/// `STALL_TIMEOUT` (no stall yet), then one sample longer, and reports the
/// stall.
fn drive_into_a_stall(clock: &ManualClock, notices: &Receiver<CaptureNotice>) {
    advance_watch(clock, CaptureSession::STALL_CHECK_INTERVAL);
    advance_watch(clock, CaptureSession::STALL_TIMEOUT);
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(
        notices,
        "exactly STALL_TIMEOUT without a frame is no stall yet",
    );
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
    );
}

/// Moves the watch thread on, a sample at a time, until it reports a
/// stall, within `STALL_TIMEOUT` and a few samples. The watch thread sends
/// a sample's notice before it sleeps again, so once a sleep is pending
/// after the advance, that sample's notice is there or none came.
fn advance_until_stalled(clock: &ManualClock, notices: &Receiver<CaptureNotice>) {
    for _ in 0..30 {
        advance_watch(clock, CaptureSession::STALL_CHECK_INTERVAL);
        let deadline = Instant::now() + RECV;
        while clock.pending_sleepers() == 0 {
            assert!(Instant::now() < deadline, "the watch thread samples");
            std::thread::sleep(Duration::from_millis(1));
        }
        if let Ok(notice) = notices.try_recv() {
            assert_eq!(
                notice,
                CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
            );
            return;
        }
    }
    panic!("no stall reported");
}

/// The seconds of silence `CaptureSession::gap_frames` writes for `gap`.
fn gap_seconds(gap: Duration) -> f64 {
    CaptureSession::gap_frames(gap) as f64 * FRAME_SIZE as f64 / SAMPLE_RATE
}

/// A device that stops delivering mid-recording, with no notification (a
/// source's owner that hangs): once nothing came for longer than
/// `STALL_TIMEOUT` the watch thread reports the stall, the rebuild
/// restarts the backend, the gap from the last frame it saw arrive is
/// written as silence, and the recording goes on with what the restarted
/// backend delivers.
#[test]
fn a_stall_past_the_timeout_rebuilds_and_fills_the_gap() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let gap_seconds =
        gap_seconds(CaptureSession::STALL_TIMEOUT + CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds
        },
        "the gap runs from the first sample, when the last frames were seen"
    );
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(backend.starts(), 2);
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(result.statistics.gap_seconds, gap_seconds);
    assert!(!result.statistics.ended_on_device_loss);
    assert!(result.statistics.dropped_frames.is_empty());
    assert_eq!(
        master_of(&result).frame_count(),
        backend.frames_delivered() + (gap_seconds * SAMPLE_RATE).round() as usize,
        "0.5 s, the gap, then the restarted backend's second"
    );
}

/// A stall shorter than `STALL_TIMEOUT` that ends on its own costs no
/// rebuild.
#[test]
fn a_short_stall_rebuilds_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    advance_watch(&clock, CaptureSession::STALL_CHECK_INTERVAL);
    advance_watch(
        &clock,
        CaptureSession::STALL_TIMEOUT.saturating_sub(CaptureSession::STALL_CHECK_INTERVAL),
    );
    assert!(clock.wait_for_sleepers(1));
    backend.resume_delivery();
    backend.wait_until_finished();
    advance_watch(&clock, CaptureSession::STALL_CHECK_INTERVAL);
    advance_watch(&clock, CaptureSession::STALL_TIMEOUT);
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "no rebuild");
    let result = session.stop().unwrap();
    assert_eq!(backend.starts(), 1);
    assert_eq!(result.statistics.device_changes, 0);
    assert_eq!(result.statistics.duration, 1.0);
}

/// Before the recording's first frame a stream that delivers nothing is
/// not stalled over a backend that may wait for playback (a Mac call
/// capture without the capture permission), however long it stays so;
/// over any other backend it is, once `STALL_TIMEOUT` passed.
#[test]
fn only_a_capture_that_waits_for_playback_may_start_without_a_frame() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.0).waits_for_playback(true),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    for _ in 0..3 {
        advance_watch(&clock, CaptureSession::STALL_TIMEOUT);
    }
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "it waits for something to play: no stall");
    assert_eq!(backend.starts(), 1);
    session.stop().unwrap();

    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.0)));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    advance_watch(&clock, CaptureSession::STALL_TIMEOUT);
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "exactly STALL_TIMEOUT is no stall yet");
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled),
        "a stream that never delivered is stalled too"
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    backend.wait_until_finished();
    session.stop().unwrap();
}

/// Over a backend that may wait for playback a restart is not held to
/// deliver at once (it may be waiting for something to play), so after the
/// recording's first frame a restarted stream that delivers nothing is a
/// stall of its own; its wait runs from the resume, not from the last
/// frame before the rebuild, and its rebuild continues the streak: the
/// second restart, after the first backoff step. Elsewhere such a restart
/// fails at once
/// (`restarts_that_start_but_deliver_nothing_are_retried_until_one_does`).
#[test]
fn a_restarted_stream_that_never_delivers_is_a_stall() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5)
            .restarts_stall_after(0.0)
            .waits_for_playback(true),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    backend.wait_until_finished();
    advance_watch(&clock, CaptureSession::STALL_TIMEOUT);
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "STALL_TIMEOUT from the resume is no stall yet");
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled),
        "the restarted stream never delivered"
    );
    advance_the_backoff(&clock, &backend, CaptureSession::RESTART_BACKOFF[0]);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 2, .. }
    ));
    let result = session.stop().unwrap();
    assert_eq!(backend.starts(), 3);
    assert_eq!(result.statistics.device_changes, 2);
}

/// Once the watch thread and a rebuild's backoff sleep, moves the clock to
/// just short of the backoff's `wait` (no restart yet), then the rest, and
/// waits for the restart it lets through.
fn advance_the_backoff(clock: &ManualClock, backend: &SyntheticCaptureBackend, wait: Duration) {
    let starts = backend.starts();
    assert!(
        clock.wait_for_sleepers(2),
        "the watch thread and the backoff"
    );
    clock.advance(wait.saturating_sub(Duration::from_millis(1)));
    assert!(clock.wait_for_sleepers(2));
    settle();
    assert_eq!(backend.starts(), starts, "no restart before {wait:?}");
    clock.advance(Duration::from_millis(1));
    let deadline = Instant::now() + RECV;
    while backend.starts() == starts {
        assert!(Instant::now() < deadline, "the restart after {wait:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Lets `backend`'s stalled producer deliver again, and waits for its
/// first frame.
fn resume_delivery_until_a_frame(backend: &SyntheticCaptureBackend) {
    let before = backend.frames_delivered();
    backend.resume_delivery();
    let deadline = Instant::now() + RECV;
    while backend.frames_delivered() == before {
        assert!(Instant::now() < deadline, "the restart delivers");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A stream that resumes and stalls again before it delivered anything (a
/// Mac call capture whose output has no other client, which resumes
/// waiting for playback) is one streak of rebuilds: the restarts count on
/// across them, back off as restarts that fail do, up to
/// `RESTART_BACKOFF_LONGEST`, and `StillRestarting` comes once when they
/// pass `RESTART_ATTEMPTS`. The warning stands through the resumes, and
/// `Delivering` ends it once the stream delivers.
#[test]
fn a_stream_that_resumes_and_stalls_over_and_over_backs_off_and_warns() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5)
            .restarts_stall_after(0.0)
            .waits_for_playback(true),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    let last = CaptureSession::RESTART_ATTEMPTS + 3;
    for attempt in 2..=last {
        backend.wait_until_finished();
        advance_until_stalled(&clock, &notices);
        if attempt == CaptureSession::RESTART_ATTEMPTS + 1 {
            assert_eq!(
                notices.recv_timeout(RECV).unwrap(),
                CaptureNotice::StillRestarting {
                    attempt: CaptureSession::RESTART_ATTEMPTS
                }
            );
        }
        advance_the_backoff(
            &clock,
            &backend,
            CaptureSession::restart_backoff(attempt - 1),
        );
        assert!(
            matches!(
                notices.recv_timeout(RECV).unwrap(),
                CaptureNotice::DeviceResumed { attempt: resumed, .. } if resumed == attempt
            ),
            "attempt {attempt}"
        );
    }
    assert_eq!(
        CaptureSession::restart_backoff(last - 1),
        CaptureSession::RESTART_BACKOFF_LONGEST,
        "the backoff reached its longest"
    );
    backend.wait_until_finished();
    assert_no_notice(&notices, "the warning stands through the resumes");
    resume_delivery_until_a_frame(&backend);
    advance_watch(&clock, CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::Delivering,
        "the first frames end the warning"
    );
    let result = session.stop().unwrap();
    assert_eq!(backend.starts(), 1 + last);
    assert_eq!(result.statistics.device_changes, last);
    assert!(!result.statistics.ended_on_device_loss);
}

/// Restarts that start but never offer a frame back off between their
/// waits for one: each try waits `STALL_TIMEOUT`, then the backoff, which
/// doubles up to `RESTART_BACKOFF_LONGEST` and stays there.
#[test]
fn restarts_that_never_offer_a_frame_back_off_up_to_the_longest_wait() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_stall_after(0.0),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    // Every wait is a multiple of 50 ms, so stepping by it lands on each
    // restart's start.
    let step = Duration::from_millis(50);
    assert!(clock.wait_for_sleepers(2), "the first restart's wait");
    let mut seen = backend.starts();
    assert_eq!(seen, 2, "the first restart starts at the stall");
    let mut started_at = vec![clock.now()];
    for _ in 0..2_000 {
        assert!(
            clock.wait_for_sleepers(2),
            "the watch thread and the rebuild"
        );
        if backend.starts() != seen {
            seen = backend.starts();
            started_at.push(clock.now());
            if started_at.len() == 7 {
                break;
            }
        }
        clock.advance(step);
    }
    let between: Vec<Duration> = started_at
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    let expected: Vec<Duration> = (1..=6)
        .map(|attempt| CaptureSession::STALL_TIMEOUT + CaptureSession::restart_backoff(attempt))
        .collect();
    assert_eq!(between, expected);
    assert_eq!(
        notices.try_recv().ok(),
        Some(CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS
        })
    );
    assert_no_notice(&notices, "said once, nothing resumed");
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.duration, 0.5);
    assert!(clock.wait_for_sleepers(0));
}

/// A stop while a restart waits for its first frame, or while the rebuild
/// waits out the backoff, cancels the wait and returns the recording at
/// once, well within one start's deadline, leaving nothing asleep.
#[test]
fn a_stop_during_the_wait_for_a_first_frame_or_the_backoff_returns_at_once() {
    for in_the_backoff in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new());
        let backend = Arc::new(SyntheticCaptureBackend::new(
            stalling(1.0, 0.5).restarts_stall_after(0.0),
        ));
        let session = Arc::new(in_person_session(
            directory.path(),
            backend.clone(),
            clock.clone(),
        ));
        let notices = session.notices();
        session.start(Uuid::new_v4()).unwrap();
        backend.wait_until_finished();
        drive_into_a_stall(&clock, &notices);
        assert!(clock.wait_for_sleepers(2), "the first restart's wait");
        assert_eq!(backend.starts(), 2);
        if in_the_backoff {
            // The wait gives up `STALL_TIMEOUT` after the start; the
            // backoff follows.
            for _ in 0..CaptureSession::STALL_TIMEOUT.as_millis() / 100 {
                assert!(clock.wait_for_sleepers(2));
                clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
            }
            assert!(clock.wait_for_sleepers(2), "the backoff");
        }
        let stopping = Arc::clone(&session);
        let began = Instant::now();
        let result = within_deadline("the stop", move || stopping.stop()).unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "within one start's deadline: {:?}",
            began.elapsed()
        );
        assert!(clock.wait_for_sleepers(0), "nothing left asleep");
        assert_eq!(session.state(), CaptureState::Idle);
        assert_eq!(result.statistics.duration, 0.5);
        assert!(!result.statistics.ended_on_device_loss);
        assert_eq!(backend.starts(), 2, "in the backoff: {in_the_backoff}");
    }
}

/// Moves the clock a sample at a time while the watch thread and the
/// rebuild both sleep on it, until `done`; panics after `limit` samples.
fn advance_the_rebuild_until(clock: &ManualClock, limit: usize, mut done: impl FnMut() -> bool) {
    for _ in 0..limit {
        if done() {
            return;
        }
        assert!(
            clock.wait_for_sleepers(2),
            "the watch thread and the rebuild sleep"
        );
        clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    }
    assert!(done(), "not within {limit} samples");
}

/// A stall whose restarts start but deliver nothing, with no chosen
/// microphone to fall back from (a clock master that hangs on the Mac or
/// on Windows, where such a device still starts): each restart that offers
/// no frame within `STALL_TIMEOUT` of its start is stopped and counts as
/// one whose graph did not run, so the restarts take the backoff, go on
/// past `RESTART_ATTEMPTS` with `StillRestarting` (the recorder's warning),
/// and resume on the first one that delivers, the gap written once.
#[test]
fn restarts_that_start_but_deliver_nothing_are_retried_until_one_does() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_stall_after(0.0),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let mut still = None;
    advance_the_rebuild_until(&clock, 200, || {
        still = notices.try_recv().ok();
        still.is_some()
    });
    assert_eq!(
        still,
        Some(CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS
        }),
        "no restart delivered, none resumed"
    );
    assert_eq!(backend.starts(), 1 + CaptureSession::RESTART_ATTEMPTS);
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    // The next restart starts after the backoff; it delivers once asked.
    let restart = 1 + CaptureSession::RESTART_ATTEMPTS + 1;
    advance_the_rebuild_until(&clock, 30, || backend.starts() == restart);
    assert!(clock.wait_for_sleepers(2), "its wait for a first frame");
    resume_delivery_until_a_frame(&backend);
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: CaptureSession::RESTART_ATTEMPTS + 1,
            gap_seconds: gap_seconds(CaptureSession::MAXIMUM_GAP)
        },
        "the gap runs past MAXIMUM_GAP and is capped there"
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(backend.starts(), restart);
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(
        result.statistics.gap_seconds,
        gap_seconds(CaptureSession::MAXIMUM_GAP)
    );
    assert_eq!(
        result.statistics.duration,
        0.5 + gap_seconds(CaptureSession::MAXIMUM_GAP) + 1.0,
        "before the stall, the gap, the restart that delivered"
    );
}

/// A restart whose stream delivers during its wait for a first frame ends
/// the gap at the wait's last sample that saw nothing, not at the start's
/// return: the gap is the time no audio came, to within a sample.
#[test]
fn a_restart_that_delivers_during_its_wait_ends_the_gap_at_its_last_empty_sample() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_stall_after(0.0),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(clock.wait_for_sleepers(2), "the first restart's wait");
    assert_eq!(backend.starts(), 2);
    // A second into the wait, well past the start's return.
    for _ in 0..10 {
        assert!(clock.wait_for_sleepers(2));
        clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    }
    assert!(clock.wait_for_sleepers(2));
    let last_empty = clock.now();
    resume_delivery_until_a_frame(&backend);
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    // The watch thread last saw a frame at its first sample.
    let gap = gap_seconds(last_empty.saturating_sub(CaptureSession::STALL_CHECK_INTERVAL));
    let resumed = loop {
        if let CaptureNotice::DeviceResumed { gap_seconds, .. } =
            notices.recv_timeout(RECV).unwrap()
        {
            break gap_seconds;
        }
    };
    assert_eq!(resumed, gap);
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, gap);
}

/// A change reported while a restart waits for a first frame it never
/// gets is dropped with that start, since the next start reads the devices
/// as they are: the restart after it resumes, and no rebuild follows.
#[test]
fn a_change_during_a_restart_that_delivers_nothing_costs_no_extra_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_stall_after(0.0),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(clock.wait_for_sleepers(2), "the first restart's wait");
    assert_eq!(backend.starts(), 2);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    settle();
    advance_the_rebuild_until(&clock, 200, || backend.starts() == 3);
    assert!(clock.wait_for_sleepers(2), "the second restart's wait");
    resume_delivery_until_a_frame(&backend);
    clock.advance(CaptureSession::STALL_CHECK_INTERVAL);
    loop {
        if let CaptureNotice::DeviceResumed { attempt, .. } = notices.recv_timeout(RECV).unwrap() {
            assert_eq!(attempt, 2);
            break;
        }
    }
    settle();
    assert_eq!(backend.starts(), 3, "no rebuild after the resume");
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 1);
}

/// The synthetic backend, whose restarts return `start_takes` on `clock`
/// after their first callback, as a PipeWire start reads its latency after
/// the first cycle.
struct SlowRestart {
    inner: SyntheticCaptureBackend,
    clock: Arc<ManualClock>,
    start_takes: Duration,
    starts: AtomicUsize,
}

impl CaptureBackend for SlowRestart {
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let stream = self.inner.start(lanes, input_device_uid, sink)?;
        if self.starts.fetch_add(1, Ordering::SeqCst) > 0 {
            self.clock.advance(self.start_takes);
        }
        Ok(stream)
    }

    fn stop(&self) {
        self.inner.stop();
    }

    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        self.inner.delivers_continuously(lanes)
    }

    fn waits_for_playback(&self, lanes: &[AudioLane]) -> bool {
        self.inner.waits_for_playback(lanes)
    }
}

/// Frames that arrive while a restart's `start` runs end the gap at the
/// clock's reading before that start, not at its return: the gap is the
/// time no audio came, and the master does not hold the start's latency
/// twice.
#[test]
fn frames_during_a_slow_restart_end_the_gap_where_they_began() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SlowRestart {
        inner: SyntheticCaptureBackend::new(stalling(1.0, 0.5)),
        clock: clock.clone(),
        start_takes: Duration::from_millis(500),
        starts: AtomicUsize::new(0),
    });
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    // The restart starts at once, at the stall's report one sample past
    // `STALL_TIMEOUT` after the last frame, which the first sample saw.
    let gap = gap_seconds(CaptureSession::STALL_TIMEOUT + CaptureSession::STALL_CHECK_INTERVAL);
    let resumed = loop {
        if let CaptureNotice::DeviceResumed { gap_seconds, .. } =
            notices.recv_timeout(RECV).unwrap()
        {
            break gap_seconds;
        }
    };
    assert_eq!(resumed, gap, "not the start's 0.5 s on top");
    backend.inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, gap);
    assert_eq!(result.statistics.duration, 0.5 + gap + 1.0);
}

/// A stall whose restarts fail with an error that is not `DidNotRun` (a
/// Mac start while `coreaudiod` is still gone, a Bluetooth device still
/// switching): the watchdog caught an outage the backend did not report,
/// so the restarts go on past the last attempt, with `StillRestarting`,
/// and the recording resumes once one starts, never ending `DeviceLost`.
/// Restarts that never start are cancelled by the stop, which returns the
/// recording.
#[test]
fn a_stall_whose_restarts_fail_goes_on_until_one_starts() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let failures = CaptureSession::RESTART_ATTEMPTS + 2;
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_that_fail(failures),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    for step in backoff_past_the_attempts(failures - CaptureSession::RESTART_BACKOFF.len()) {
        assert!(
            clock.wait_for_sleepers(2),
            "the watch thread and the backoff"
        );
        assert!(matches!(session.state(), CaptureState::Recording { .. }));
        clock.advance(step);
    }
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS
        }
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == failures + 1
    ));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(backend.starts(), 1 + failures + 1);

    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_that_fail(usize::MAX),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    for step in backoff_past_the_attempts(3) {
        assert!(clock.wait_for_sleepers(2));
        clock.advance(step);
    }
    assert!(clock.wait_for_sleepers(2), "still restarting");
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.duration, 0.5);
    assert!(clock.wait_for_sleepers(0), "the stop cancelled the backoff");
}

/// Silence is delivery: a lane of zeros that keeps arriving is never
/// taken for a stall, however long the recording runs.
#[test]
fn a_silent_lane_that_delivers_is_never_a_stall() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let signals = BTreeMap::from([(AudioLane::Mixed, SyntheticLane::SILENCE)]);
    let backend = Arc::new(SyntheticCaptureBackend::new(
        SyntheticOptions::signals(signals, 30.0)
            .real_time(true)
            .delivers_continuously(true),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    for _ in 0..5 {
        advance_while_delivering(
            &clock,
            || backend.frames_delivered(),
            CaptureSession::STALL_TIMEOUT,
        );
    }
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "10 s of zeros, no rebuild");
    assert_eq!(backend.starts(), 1);
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 0);
}

/// The waits after the first `RESTART_BACKOFF.len() + extra` failed
/// restarts of a rebuild, from `CaptureSession::restart_backoff`.
fn backoff_past_the_attempts(extra: usize) -> Vec<Duration> {
    (1..=CaptureSession::RESTART_BACKOFF.len() + extra)
        .map(CaptureSession::restart_backoff)
        .collect()
}

/// The backoff between restarts: `RESTART_BACKOFF` between the first
/// `RESTART_ATTEMPTS`, then twice its last step, then
/// `RESTART_BACKOFF_LONGEST` every time, however long the restarts go on.
#[test]
fn the_backoff_doubles_up_to_four_seconds_and_stays_there() {
    let ms = Duration::from_millis;
    assert_eq!(
        backoff_past_the_attempts(4),
        [
            ms(250),
            ms(500),
            ms(1_000),
            ms(2_000),
            ms(4_000),
            ms(4_000),
            ms(4_000)
        ]
    );
    assert_eq!(CaptureSession::RESTART_BACKOFF_LONGEST, ms(4_000));
    assert_eq!(
        CaptureSession::restart_backoff(usize::MAX),
        CaptureSession::RESTART_BACKOFF_LONGEST
    );
}

/// A stall whose graph never runs again (every restart fails with
/// `DidNotRun`): the restarts go on past `RESTART_ATTEMPTS`, backing off
/// up to `RESTART_BACKOFF_LONGEST`, `notices` says so once, the recording stays
/// `Recording` instead of ending `DeviceLost`, and the stop returns it
/// with everything delivered before the stall.
#[test]
fn a_stall_that_never_resumes_keeps_retrying_until_the_stop() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5)
            .restarts_that_fail(usize::MAX)
            .restarts_do_not_run(),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let steps = backoff_past_the_attempts(3);
    for step in &steps {
        assert!(
            clock.wait_for_sleepers(2),
            "the watch thread and the rebuild's backoff"
        );
        clock.advance(*step);
    }
    assert!(clock.wait_for_sleepers(2));
    assert!(matches!(session.state(), CaptureState::Recording { .. }));
    assert_eq!(
        backend.starts(),
        1 + steps.len() + 1,
        "the start and every restart so far"
    );
    assert_eq!(
        notices.try_recv().ok(),
        Some(CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS
        })
    );
    assert_no_notice(&notices, "said once");
    let result = session.stop().unwrap();
    assert_eq!(session.state(), CaptureState::Idle);
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.duration, 0.5);
    assert!(clock.wait_for_sleepers(0));
}

/// A device change whose restarts keep failing with `DidNotRun` past the
/// last attempt is retried until one runs, then resumes there.
#[test]
fn a_graph_that_does_not_run_at_the_last_restart_is_retried_until_it_runs() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let failures = CaptureSession::RESTART_ATTEMPTS + 2;
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 0.5)
            .change_device_after(0.2)
            .restarts_that_fail(failures)
            .restarts_do_not_run(),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    for step in backoff_past_the_attempts(failures - CaptureSession::RESTART_BACKOFF.len()) {
        assert!(clock.wait_for_sleepers(1), "the backoff");
        clock.advance(step);
    }
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS
        }
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == failures + 1
    ));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 1);
}

/// After a restart of the audio service (`coreaudiod` on the Mac, which
/// takes a while to come back) every failure is retried, not only
/// `DidNotRun`: restarts that fail past the last attempt go on until one
/// runs. The same failures after another change end the recording.
#[test]
fn a_restart_of_the_audio_service_is_retried_until_it_runs() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let failures = CaptureSession::RESTART_ATTEMPTS + 2;
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 0.5).restarts_that_fail(failures),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    session.device_changed(DeviceChangeReason::AudioServiceRestarted);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::AudioServiceRestarted)
    );
    for step in backoff_past_the_attempts(failures - CaptureSession::RESTART_BACKOFF.len()) {
        assert!(clock.wait_for_sleepers(1), "the backoff");
        clock.advance(step);
    }
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::StillRestarting { .. }
    ));
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == failures + 1
    ));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
}

/// The watch thread judges nothing while a rebuild runs: restarts that
/// take longer than `STALL_TIMEOUT` are no second stall, and the
/// recording resumes once, with no rebuild after it.
#[test]
fn a_rebuild_longer_than_the_stall_timeout_is_not_taken_for_a_stall() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let failures = CaptureSession::RESTART_ATTEMPTS;
    let backend = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5)
            .restarts_that_fail(failures)
            .restarts_do_not_run(),
    ));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let steps = backoff_past_the_attempts(failures - CaptureSession::RESTART_BACKOFF.len());
    assert!(steps.iter().sum::<Duration>() > CaptureSession::STALL_TIMEOUT);
    for step in steps {
        assert!(
            clock.wait_for_sleepers(2),
            "the watch thread and the backoff"
        );
        clock.advance(step);
    }
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::StillRestarting { .. }
    ));
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == failures + 1
    ));
    backend.wait_until_finished();
    settle();
    // The watch thread may have sampled the restarted stream's frames
    // after the resume, which ends the warning.
    let rest: Vec<CaptureNotice> = notices.try_iter().collect();
    assert!(
        rest.iter()
            .all(|notice| *notice == CaptureNotice::Delivering),
        "no stall was judged while the rebuild ran: {rest:?}"
    );
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(backend.starts(), 1 + failures + 1);
}

/// The gap of a rebuild runs from the last frame the watch thread saw
/// arrive, not from the report: a change reported 0.5 s after the frames
/// stopped (a backend's coalescing delay) leaves no 0.5 s hole in the
/// master's timeline.
#[test]
fn a_gap_runs_from_the_last_frame_the_watch_thread_saw() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    advance_watch(&clock, CaptureSession::STALL_CHECK_INTERVAL);
    advance_watch(&clock, Duration::from_millis(500));
    assert!(clock.wait_for_sleepers(1));
    session.device_changed(DeviceChangeReason::InputDeviceGone);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::InputDeviceGone)
    );
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 0.5
        }
    );
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, 0.5);
    assert_eq!(result.statistics.duration, 2.0);
}

/// A chosen microphone that stalls again soon after the rebuild resumed
/// on it is not restarted once more, to stall again: the next rebuild
/// records the default input in its place, marked as the fallback.
#[test]
fn a_chosen_microphone_that_stalls_again_soon_is_replaced_by_the_default() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::over(
        tones(&[AudioLane::Mixed], 1.0)
            .stall_after(0.5)
            .restarts_stall_after(0.5),
    ));
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    let input = || session.stream().and_then(|stream| stream.input);
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(input(), ChosenOrDefault::chosen(), "restarted once");
    backend.inner.wait_until_finished();
    advance_until_stalled(&clock, &notices);
    // The streak goes on: its second restart, after the first backoff step.
    advance_the_backoff(&clock, &backend.inner, CaptureSession::RESTART_BACKOFF[0]);
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 2, .. }
    ));
    assert_eq!(input(), ChosenOrDefault::fallback(), "then the default");
    assert_eq!(
        backend.asked(),
        vec![
            Some(ChosenOrDefault::CHOSEN.to_owned()),
            Some(ChosenOrDefault::CHOSEN.to_owned()),
            None
        ]
    );
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 2);
}

/// Over a backend whose streams may wait for playback (a Mac call capture
/// without the capture permission, which stops whenever playback does) a
/// chosen microphone that stalls again soon is not given up for the
/// default: the stall says nothing about the microphone.
#[test]
fn a_capture_that_waits_for_playback_keeps_the_chosen_microphone_through_stalls() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::over(
        tones(&[AudioLane::Mixed], 1.0)
            .stall_after(0.5)
            .restarts_stall_after(0.5)
            .waits_for_playback(true),
    ));
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    let input = || session.stream().and_then(|stream| stream.input);
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    for rebuild in 1..=2 {
        if rebuild == 2 {
            advance_until_stalled(&clock, &notices);
            advance_the_backoff(&clock, &backend.inner, CaptureSession::RESTART_BACKOFF[0]);
        }
        assert!(matches!(
            notices.recv_timeout(RECV).unwrap(),
            CaptureNotice::DeviceResumed { attempt, .. } if attempt == rebuild
        ));
        assert_eq!(input(), ChosenOrDefault::chosen(), "rebuild {rebuild}");
        backend.inner.wait_until_finished();
    }
    assert_eq!(
        backend.asked(),
        vec![Some(ChosenOrDefault::CHOSEN.to_owned()); 3],
        "the chosen microphone every time"
    );
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 2);
}

/// Waits until `backend` answered `count` probes in all.
fn wait_for_probes(backend: &ChosenOrDefault, count: usize) {
    let deadline = Instant::now() + RECV;
    while backend.probed.load(Ordering::SeqCst) < count {
        assert!(Instant::now() < deadline, "probe {count}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A chosen microphone that did not open at the start, over a backend
/// that can ask one on a stream of its own: the session records the
/// default in its place and asks after `CHOSEN_INPUT_RECHECK`, then twice
/// as long after each ask. Asks that find it still not delivering cost the
/// recording nothing (no restart, no gap, every frame, the master as long
/// as wall time); nothing else is asked of the backend.
#[test]
fn asks_that_find_the_chosen_microphone_still_silent_cost_the_recording_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::watched(60.0));
    backend.probes.store(true, Ordering::Relaxed);
    backend.opens.store(false, Ordering::Relaxed);
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    let delivered = || backend.inner.frames_delivered();
    let started = Instant::now();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::fallback()
    );
    let step = CaptureSession::STALL_CHECK_INTERVAL;
    let mut wait = CaptureSession::CHOSEN_INPUT_RECHECK;
    // How far the clock already is past the latest ask.
    let mut past_the_ask = Duration::ZERO;
    for ask in 1..=3 {
        advance_while_delivering(&clock, delivered, wait.saturating_sub(step + past_the_ask));
        assert_no_notice(&notices, "not due yet");
        assert_eq!(
            backend.probed.load(Ordering::SeqCst),
            ask - 1,
            "not due yet"
        );
        advance_while_delivering(&clock, delivered, step);
        wait_for_probes(&backend, ask);
        advance_while_delivering(&clock, delivered, step);
        assert_no_notice(&notices, "the chosen microphone did not deliver");
        past_the_ask = step;
        wait *= 2;
    }
    let wall = started.elapsed().as_secs_f64();
    let result = session.stop().unwrap();
    assert_eq!(
        backend.asked(),
        ChosenOrDefault::chosen_then_default(0),
        "no restart"
    );
    assert_eq!(backend.inner.starts(), 1);
    let statistics = &result.statistics;
    assert_eq!(statistics.device_changes, 0);
    assert_eq!(statistics.gap_seconds, 0.0);
    assert!(statistics.dropped_frames.is_empty());
    let delivered = backend.inner.frames_delivered();
    assert_eq!(
        master_of(&result).frame_count(),
        delivered - delivered % FRAME_SIZE,
        "every whole frame delivered, nothing else"
    );
    assert!(
        (statistics.duration - wall).abs() < 0.1,
        "the master: {} s against {wall} s of wall time",
        statistics.duration
    );
}

/// The ask that finds the chosen microphone delivering reports
/// `ChosenInputRecheck`, whose rebuild returns to it; on the chosen one
/// nothing asks again.
#[test]
fn an_ask_that_finds_the_chosen_microphone_delivering_returns_to_it() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::watched(30.0));
    backend.probes.store(true, Ordering::Relaxed);
    backend.opens.store(false, Ordering::Relaxed);
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    let input = || session.stream().and_then(|stream| stream.input);
    let delivered = || backend.inner.frames_delivered();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(input(), ChosenOrDefault::fallback());
    backend.opens.store(true, Ordering::Relaxed);
    advance_while_delivering(&clock, delivered, CaptureSession::CHOSEN_INPUT_RECHECK);
    wait_for_probes(&backend, 1);
    let deadline = Instant::now() + RECV;
    let changed = loop {
        advance_while_delivering(&clock, delivered, CaptureSession::STALL_CHECK_INTERVAL);
        if let Ok(notice) = notices.recv_timeout(Duration::from_millis(50)) {
            break notice;
        }
        assert!(Instant::now() < deadline, "the probe's answer");
    };
    assert_eq!(
        changed,
        CaptureNotice::DeviceChanged(DeviceChangeReason::ChosenInputRecheck)
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(input(), ChosenOrDefault::chosen(), "back on the chosen one");

    advance_while_delivering(
        &clock,
        delivered,
        CaptureSession::CHOSEN_INPUT_RECHECK_LONGEST,
    );
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "on the chosen microphone nothing asks");
    assert_eq!(backend.probed.load(Ordering::SeqCst), 1);
    let result = session.stop().unwrap();
    let statistics = &result.statistics;
    assert_eq!(statistics.device_changes, 1);
    assert!(!statistics.ended_on_device_loss);
    assert!(statistics.dropped_frames.is_empty());
    // The rebuild's dead time passes in wall time but not on the manual
    // clock the gap is measured on, so the master is checked against what
    // both starts delivered, plus the gap's silence.
    let delivered = backend.inner.frames_delivered();
    let gap = (statistics.gap_seconds * SAMPLE_RATE).round() as usize;
    assert_eq!(
        master_of(&result).frame_count(),
        delivered - delivered % FRAME_SIZE + gap,
        "every whole frame delivered through the return, and the gap"
    );
}

/// Over a backend that cannot ask a microphone without touching the
/// recording (the Mac's and Windows'), the session never asks on a timer:
/// the chosen microphone it replaced is asked for again only by the rebuild
/// of a device change.
#[test]
fn a_backend_that_cannot_probe_is_never_asked_on_a_timer() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(ChosenOrDefault::watched(30.0));
    backend.opens.store(false, Ordering::Relaxed);
    let session = backend.session(directory.path(), clock.clone());
    let notices = session.notices();
    let delivered = || backend.inner.frames_delivered();
    session.start(Uuid::new_v4()).unwrap();
    for _ in 0..3 {
        advance_while_delivering(
            &clock,
            delivered,
            CaptureSession::CHOSEN_INPUT_RECHECK_LONGEST,
        );
    }
    assert!(clock.wait_for_sleepers(1));
    assert_no_notice(&notices, "no ask");
    assert_eq!(backend.probed.load(Ordering::SeqCst), 0);
    assert_eq!(backend.asked(), ChosenOrDefault::chosen_then_default(0));
    backend.opens.store(true, Ordering::Relaxed);
    session.device_changed(DeviceChangeReason::DefaultOutputChanged);
    assert_eq!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultOutputChanged)
    );
    assert!(matches!(
        notices.recv_timeout(RECV).unwrap(),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    assert_eq!(
        session.stream().and_then(|stream| stream.input),
        ChosenOrDefault::chosen(),
        "the device change's rebuild asked for it"
    );
    session.stop().unwrap();
}

// Echo cancellation in the real processing path (LiveAECPathTests)

fn record(
    signals: BTreeMap<AudioLane, SyntheticLane>,
    seconds: f64,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(SyntheticOptions::signals(
        signals, seconds,
    )));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), true),
        backend.clone(),
        None,
        1_500,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert!(result.statistics.dropped_frames.is_empty());
    let master = master_of(&result);
    let raw = CafFile::read(
        &RecordingLayout::from_asset(&result.asset)
            .unwrap()
            .directory
            .join("mic.raw.caf"),
    )
    .unwrap();
    (
        raw.channels[0].clone(),
        master.channels[0].clone(),
        master.channels[1].clone(),
    )
}

#[test]
fn echo_of_the_system_lane_is_cancelled_on_the_mic_lane() {
    let (raw, processed, system) = record(
        [
            (
                AudioLane::Mic,
                SyntheticLane::new(0.0, 0.0).with_echo(AudioLane::System, 0.060, 0.5),
            ),
            (AudioLane::System, SyntheticLane::new(1_000.0, 0.5)),
        ]
        .into_iter()
        .collect(),
        4.0,
    );
    let range = 2 * 48_000..4 * 48_000;
    let erle = EchoMetrics::erle(&raw, &processed, range.clone());
    assert!(erle >= 15.0, "ERLE {erle} dB over the last two seconds");
    assert_eq!(raw.len(), processed.len());
    assert!((EchoMetrics::decibels(EchoMetrics::rms(&raw[range.clone()])) - -15.05).abs() < 0.2);
    assert!((EchoMetrics::decibels(EchoMetrics::rms(&system[range])) - -9.03).abs() < 0.2);
}

#[test]
fn the_independent_tone_survives_within_three_decibels() {
    let (raw, processed, _) = record(
        [
            (
                AudioLane::Mic,
                SyntheticLane::new(320.0, 0.25).with_echo(AudioLane::System, 0.060, 0.5),
            ),
            (AudioLane::System, SyntheticLane::new(1_000.0, 0.5)),
        ]
        .into_iter()
        .collect(),
        4.0,
    );
    let range = 2 * 48_000..4 * 48_000;
    let before = EchoMetrics::tone_level(&raw[range.clone()], 320.0, 48_000.0);
    let after = EchoMetrics::tone_level(&processed[range.clone()], 320.0, 48_000.0);
    let change = 20.0 * (after / before).log10();
    assert!(change.abs() <= 3.0, "own tone changed by {change} dB");
    assert!((before - 0.25).abs() < 0.01);

    let echo_before = EchoMetrics::tone_level(&raw[range.clone()], 1_000.0, 48_000.0);
    let echo_after = EchoMetrics::tone_level(&processed[range], 1_000.0, 48_000.0);
    let erle = 20.0 * (echo_before / echo_after).log10();
    assert!(erle >= 15.0, "echo tone ERLE {erle} dB under double talk");
}

/// The backend as the session drives it, noting whether the session's
/// [`Playback`] gate is held at each `start` and `stop`: the hold must be
/// taken before the backend (on macOS its tap) exists and kept until the
/// backend has stopped.
struct GateWatch {
    inner: SyntheticCaptureBackend,
    playback: Playback,
    held_at_start: Mutex<Vec<bool>>,
    held_at_stop: Mutex<Vec<bool>>,
}

impl GateWatch {
    fn new(options: SyntheticOptions, playback: &Playback) -> Arc<Self> {
        Arc::new(Self {
            inner: SyntheticCaptureBackend::new(options),
            playback: playback.clone(),
            held_at_start: Mutex::new(Vec::new()),
            held_at_stop: Mutex::new(Vec::new()),
        })
    }

    fn session(self: &Arc<Self>, directory: &Path) -> CaptureSession {
        CaptureSession::with_backend(
            configuration(CaptureMode::Call, directory, false),
            self.clone(),
            passthrough(),
            1_000,
            Arc::new(SystemClock::new()),
        )
        .unwrap()
        .with_playback(self.playback.clone())
    }
}

impl CaptureBackend for GateWatch {
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        self.held_at_start
            .lock()
            .unwrap()
            .push(self.playback.is_recording());
        self.inner.start(lanes, input_device_uid, sink)
    }

    fn stop(&self) {
        self.held_at_stop
            .lock()
            .unwrap()
            .push(self.playback.is_recording());
        self.inner.stop();
    }
}

/// No in-app playback while recording: the session holds the gate from
/// before its backend starts, across a rebuild, until the backend has
/// stopped; playback is refused meanwhile and allowed again after.
#[test]
fn playback_is_refused_while_the_session_records_and_across_a_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let playback = Playback::new();
    let backend = GateWatch::new(tones(&call(), 0.5), &playback);
    let session = backend.session(directory.path());
    let notices = session.notices();
    assert!(playback.begin(|| {}).is_ok(), "idle: playback is allowed");

    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(playback.begin(|| {}).unwrap_err(), PlaybackRefused);
    session.device_changed(DeviceChangeReason::DefaultOutputChanged);
    loop {
        let notice = notices.recv_timeout(RECV).unwrap();
        if matches!(notice, CaptureNotice::DeviceResumed { .. }) {
            break;
        }
    }
    assert_eq!(
        playback.begin(|| {}).unwrap_err(),
        PlaybackRefused,
        "still refused after the rebuild"
    );
    session.stop().unwrap();

    assert!(playback.begin(|| {}).is_ok(), "allowed once stopped");
    assert_eq!(*backend.held_at_start.lock().unwrap(), [true, true]);
    assert!(
        backend
            .held_at_stop
            .lock()
            .unwrap()
            .iter()
            .all(|held| *held),
        "held through every stop of the backend"
    );
}

/// Playback that runs when a recording starts is stopped before the
/// backend starts.
#[test]
fn playback_that_runs_is_stopped_when_the_session_starts() {
    let directory = tempfile::tempdir().unwrap();
    let playback = Playback::new();
    let backend = GateWatch::new(tones(&call(), 0.5), &playback);
    let session = backend.session(directory.path());
    let stopped = Arc::new(AtomicBool::new(false));
    let on_stop = Arc::clone(&stopped);
    let permit = playback
        .begin(move || on_stop.store(true, Ordering::SeqCst))
        .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    assert!(stopped.load(Ordering::SeqCst));
    assert!(permit.is_stopped());
    session.stop().unwrap();
}

/// A start that fails, or panics once the backend runs, releases the gate:
/// playback is allowed again, and the failed state is no reason to refuse.
#[test]
fn a_start_that_fails_or_panics_releases_the_gate() {
    let directory = tempfile::tempdir().unwrap();
    let playback = Playback::new();
    let refused = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        Arc::new(Failing),
        passthrough(),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap()
    .with_playback(playback.clone());
    assert!(refused.start(Uuid::new_v4()).is_err());
    assert!(!playback.is_recording(), "released after a failed start");

    let backend = GateWatch::new(tones(&call(), 0.5).start_panics_once_running(), &playback);
    let session = backend.session(directory.path());
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.start(Uuid::new_v4())
    }));
    assert!(started.is_err(), "the start panicked");
    assert!(!playback.is_recording(), "released after a panicking start");
    assert_eq!(
        *backend.held_at_stop.lock().unwrap(),
        [true],
        "released only after the guard stopped the backend"
    );
    assert!(playback.begin(|| {}).is_ok());
}

/// A session made without `with_playback` holds the process's gate, the
/// one every player asks ([`Playback::global`]), while it records. Only
/// the held state is asserted: the other tests' sessions share that gate.
#[test]
fn a_session_holds_the_process_gate_while_it_records() {
    let directory = tempfile::tempdir().unwrap();
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        Arc::new(SyntheticCaptureBackend::new(tones(&call(), 0.5))),
        passthrough(),
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    assert!(Playback::global().is_recording());
    assert_eq!(
        Playback::global().begin(|| {}).unwrap_err(),
        PlaybackRefused
    );
    session.stop().unwrap();
}

/// Two sessions on one gate: it opens only when both have stopped.
#[test]
fn two_recording_sessions_keep_the_gate_shut_until_both_stop() {
    let playback = Playback::new();
    let (first_directory, second_directory) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let first = GateWatch::new(tones(&call(), 0.5), &playback).session(first_directory.path());
    let second = GateWatch::new(tones(&call(), 0.5), &playback).session(second_directory.path());
    first.start(Uuid::new_v4()).unwrap();
    second.start(Uuid::new_v4()).unwrap();
    first.stop().unwrap();
    assert_eq!(playback.begin(|| {}).unwrap_err(), PlaybackRefused);
    second.stop().unwrap();
    assert!(playback.begin(|| {}).is_ok());
}
