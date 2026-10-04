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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_audio::capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureMode, CaptureNotice, CaptureResult,
    CaptureSession, CaptureState, CaptureStream, DeviceChangeReason, LaneLevel,
};
use steno_audio::realtime::LaneFrameSink;
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::testing::{ManualClock, SyntheticCaptureBackend, SyntheticLane};
use steno_audio::writer::{
    CafFile, LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting, WavFile,
};
use steno_audio::{Clock, EchoMetrics, PassthroughEchoCanceller, SAMPLE_RATE, SystemClock};
use steno_core::paths::file_url_path;
use steno_core::{AudioFormat, AudioLane, AudioRetention, EchoCanceller, RecordingLayout};
use uuid::Uuid;

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
/// readable to its last frame.
#[test]
fn device_lost_stops_cleanly_with_a_readable_master() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 10.0)
            .change_device_after(1.0)
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
    let seen = collect_states(&states, until_failed);
    assert_eq!(
        seen.last().unwrap().failure(),
        Some(&CaptureError::DeviceLost)
    );
    assert!(seen.contains(&CaptureState::Stopping));

    // The state carries the finalised partial recording; `stop()` returns
    // the same one.
    let result = session.stop().unwrap();
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
}

impl CaptureBackend for HandsOverTheSink {
    fn start(
        &self,
        _: &[AudioLane],
        _: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        *self.sink.lock().unwrap() = Some(sink);
        Ok(CaptureStream::SYNTHETIC)
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
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(HandsOverTheSink {
        sink: Mutex::new(None),
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
    assert_eq!(master_of(&result).frame_count(), accepted * 480);
    assert_eq!(
        result.statistics.dropped_frames,
        BTreeMap::from([(AudioLane::Mic, refused), (AudioLane::System, refused)])
    );
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

#[test]
fn a_failing_backend_leaves_the_session_failed_and_no_folder() {
    let directory = tempfile::tempdir().unwrap();
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::InPerson, directory.path(), false),
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
    assert!(
        !RecordingLayout::new(directory.path(), meeting_id)
            .directory
            .exists()
    );
    assert!(matches!(session.stop(), Err(CaptureError::InvalidState(_))));
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
        let before = backend.delivered();
        while backend.delivered() == before {
            std::thread::sleep(Duration::from_millis(1));
        }
        open.send(()).unwrap();
        stopper.join().unwrap()
    })
    .expect("stop() returns the recording");
    assert!(master_of(&result).frame_count() < backend.delivered());
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

/// In production the relay holds two seconds; a gap wider than that waits
/// for the writer in 5 ms steps on the clock, and every frame of silence
/// still arrives: 1.75 s of gap through a one-second relay. The old
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
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 1.0)
            .change_device_after(0.5)
            .restarts_that_fail(3)
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
