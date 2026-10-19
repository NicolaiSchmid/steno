//! The session over the synthetic backend: state machine, levels, files,
//! drop accounting, device changes and loss, with the rebuild's backoff on
//! a `ManualClock`. Waits for real work take [`HANG_GUARD`], which only
//! turns a hang into a failure; only claims that nothing arrives wait the
//! short [`settle`] window.

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
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use steno_audio::capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureInput, CaptureMode, CaptureNotice,
    CaptureSession, CaptureState, CaptureStream, DeviceChangeReason, LaneLevel,
};
use steno_audio::realtime::{FrameRelay, LaneFrameSink, Missed};
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::testing::{HANG_GUARD, ManualClock, SyntheticCaptureBackend, SyntheticLane};
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

/// The next message on `receiver`, failing as hung after [`HANG_GUARD`].
#[track_caller]
fn recv<T>(receiver: &Receiver<T>) -> T {
    match receiver.recv_timeout(HANG_GUARD) {
        Ok(message) => message,
        Err(RecvTimeoutError::Timeout) => panic!("hung: nothing came within {HANG_GUARD:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("the sender is gone"),
    }
}

/// Collects states until the predicate matches, failing as hung after
/// [`HANG_GUARD`].
#[track_caller]
fn collect_states(
    states: &Receiver<CaptureState>,
    mut done: impl FnMut(&CaptureState) -> bool,
) -> Vec<CaptureState> {
    let mut seen = Vec::new();
    loop {
        match states.recv_timeout(HANG_GUARD) {
            Ok(state) => {
                let finished = done(&state);
                seen.push(state);
                if finished {
                    return seen;
                }
            }
            Err(error) => {
                panic!("hung: no further state within {HANG_GUARD:?} ({error}): {seen:?}")
            }
        }
    }
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
/// happened. Such a claim fails only by an extra event, so this stays short.
fn settle() {
    std::thread::sleep(Duration::from_millis(100));
}

#[test]
fn idle_starting_recording_stopping_idle_over_the_synthetic_backend() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(tones(&call(), 3.0)));
    // Ten seconds of writer headroom: the backend delivers faster than a
    // debug-build writer.
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

    // Amplitude 0.5 sines are -9.03 dBFS.
    let first = recv(&levels);
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

/// A device that never comes back: four restarts fail and the recording
/// ends in `DeviceLost`, readable, with its [`Playback`] hold released.
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
        recv(&notices),
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

    // This backend's one change is spent, so the second recording sees none.
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

/// A tap that never delivers is reported through `system_lane_silent` and
/// the level floor, while the recording completes.
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
    let first = recv(&levels);
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

/// The default relay rides out a writer stalled for 20 s with nothing
/// dropped.
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

/// The real writer on a filesystem where every periodic sync fails; with
/// `close_too` the close's sync fails too.
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

/// A sync that always fails cuts nothing short: all 8 s are in the master,
/// and the recording ends `Failed` on the sync's error (a failed close
/// ranks first).
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

/// One frame of relay headroom: every frame the writer missed is counted,
/// on every lane alike.
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
    let latest = recv(&levels);
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
/// microphone: while `connected` it records it, otherwise the default,
/// saying so in the stream. A connected one that does not `opens` fails the
/// start with [`Self::chosen_failure`]; the default fails while not
/// `default_opens`. Every UID asked for is kept. With `probes` it can probe
/// a microphone on a stream of its own; `ask_hold` holds the next probe
/// until the test releases it.
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
    ask_hold: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}

impl ChosenOrDefault {
    const CHOSEN: &str = "usb-microphone";

    fn new(seconds: f64) -> Self {
        Self::over(tones(&[AudioLane::Mixed], seconds))
    }

    /// Delivering at wall-clock speed and watched, as a live backend is.
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
            ask_hold: Mutex::new(None),
        }
    }

    fn chosen_failure() -> CaptureError {
        CaptureError::BackendFailed("the chosen microphone did not open".into())
    }

    fn default_failure() -> CaptureError {
        CaptureError::BackendFailed("the default input did not open".into())
    }

    /// The UIDs asked for: the chosen one `restarts + 1` times, then the
    /// default.
    fn chosen_then_default(restarts: usize) -> Vec<Option<String>> {
        let mut asked = vec![Some(Self::CHOSEN.to_owned()); 1 + restarts];
        asked.push(None);
        asked
    }

    /// A session asking this backend for [`Self::CHOSEN`], with a 10 s relay.
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

    fn chosen() -> Option<CaptureInput> {
        Self::input(Self::CHOSEN, "USB Microphone", false)
    }

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

    fn probes_inputs(&self) -> bool {
        self.probes.load(Ordering::Relaxed)
    }

    fn probe_input(&self, uid: &str) -> bool {
        assert_eq!(uid, Self::CHOSEN);
        let hold = self.ask_hold.lock().unwrap().take();
        if let Some((began, release)) = hold {
            let _ = began.send(());
            // The test's sender drops before its session.
            let _ = release.recv();
        }
        let delivers = self.connected.load(Ordering::Relaxed) && self.opens.load(Ordering::Relaxed);
        self.probed.fetch_add(1, Ordering::SeqCst);
        delivers
    }
}

/// A chosen microphone that goes during a recording is replaced by the
/// default without `DeviceLost`; once it is back, the next change returns.
#[test]
fn a_chosen_microphone_that_goes_and_comes_back_keeps_the_recording() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(ChosenOrDefault::new(0.5));
    let session = backend.session(directory.path(), Arc::new(SystemClock::new()));
    let notices = session.notices();
    let next_notice = || recv(&notices);
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

/// A chosen microphone that does not open at the start: the recording runs
/// on the default, marked as the fallback, or fails with the chosen error.
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

/// After a failed start, `stop()` fails instead of handing out the previous
/// meeting's asset.
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

/// Each start resets the shared canceller before the first frame.
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

/// Frame counts move between rates rounded down; an unread rate leaves the
/// count alone.
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
/// one processing frame.
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

/// Wraps the real writer and fails every `write` after
/// `fail_after_frames`, and `finish()` when asked.
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

/// A disk that fills while closing: `stop()` returns the asset and the
/// state carries the failure.
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

/// A disk that fills mid-recording: `stop()` returns what was written.
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

/// A writer thread that panics: `stop()` fails with `WriterFailed` instead
/// of hanging in `Stopping`, and the next recording starts.
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

/// Stands in for the IOProc: the test pushes callbacks into the sink.
struct HandsOverTheSink {
    sink: Mutex<Option<Arc<LaneFrameSink>>>,
    stream: CaptureStream,
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

/// Passes the near end through once the gate opens; until then processing
/// is stuck in its first frame.
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

/// Callbacks while processing is stuck: the rings refuse what does not fit,
/// and the overruns reach `dropped_frames`.
#[test]
fn ring_overruns_are_reported_in_dropped_frames() {
    ring_overruns_are_reported(SAMPLE_RATE);
}

/// The same at 24 kHz.
#[test]
fn ring_overruns_at_24_khz_are_reported_in_48_khz_frames() {
    ring_overruns_are_reported(24_000.0);
}

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
    // At 24 kHz half a converter window is held back, one frame less.
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

/// A rate the converter cannot take is recorded unconverted instead of
/// panicking. 1.2 s at 4 kHz is ten whole frames.
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

/// Fills `audio` with another meeting's folder and a file of its own;
/// returns that meeting's id.
fn audio_folder_with_a_meeting(audio: &Path) -> Uuid {
    let id = Uuid::new_v4();
    let other = RecordingLayout::new(audio, id);
    std::fs::create_dir_all(&other.directory).unwrap();
    std::fs::write(other.master(AudioFormat::Caf48kFloat32), b"another master").unwrap();
    std::fs::write(other.sidecar(AudioLane::Mixed), b"another sidecar").unwrap();
    std::fs::write(audio.join("notes.txt"), b"the user's").unwrap();
    id
}

/// A start whose devices do not open removes only the folder it made.
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

/// A start for a meeting whose folder exists is refused before any device
/// opens, leaving every file as it was.
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

// Device changes

/// A device change rebuilds the backend in place: `Recording` throughout,
/// every frame of both starts on the same files, the canceller cold again.
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
    // The rebuild may already have replaced the first stream.
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert_eq!(
        recv(&notices),
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

/// Keeps every far-end sample and passes the microphone through.
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

/// A 24 kHz aggregate (a hands-free headset) still records 48 and 16 kHz
/// files, with the far end delayed by the latencies in 48 kHz frames.
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

/// A gap longer than the sink's 2 s rings is written in full as silence:
/// three failed restarts leave 3.25 s of zeros between the two devices.
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
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    advance_through_sleeps(&clock, 2);
    assert!(clock.wait_for_sleepers(1), "the third backoff");
    clock.advance(Duration::from_millis(2_500));
    assert_eq!(
        recv(&notices),
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

/// `stop()` during a backoff abandons the rebuild: one finalise, `Idle`,
/// and the clock changes nothing afterwards.
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
        recv(&notices),
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

/// Runs `f` on its own thread and fails after [`HANG_GUARD`].
fn within_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = channel();
    std::thread::spawn(move || {
        let _ = done.send(f());
    });
    match finished.recv_timeout(HANG_GUARD) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => panic!("{what} did not return within {HANG_GUARD:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

/// A 30 s outage fills `MAXIMUM_GAP` (10 s) of silence, and `gap_seconds`
/// reports that.
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
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_secs(30));
    assert_eq!(
        recv(&notices),
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

// The stall watchdog, the retry of a graph that does not run, and the ask
// for a chosen microphone the session replaced

/// An in-person session over `backend` on `clock`, with 30 s of relay.
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
/// thread's last sample (the first move may predate a rebuild's resume).
fn advance_while_delivering(clock: &ManualClock, delivered: impl Fn() -> usize, by: Duration) {
    assert!(clock.wait_for_sleepers(1), "the watch thread sleeps");
    let deadline = Instant::now() + HANG_GUARD;
    for _ in 0..2 {
        let before = delivered();
        while delivered() == before {
            assert!(Instant::now() < deadline, "the backend delivers");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    clock.advance(by);
}

fn assert_no_notice(notices: &Receiver<CaptureNotice>, context: &str) {
    settle();
    assert_eq!(notices.try_recv().ok(), None, "{context}");
}

/// Drives the watch thread over a stalled backend to just under
/// `STALL_TIMEOUT` (no stall yet), then one sample on, to the stall.
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
        recv(notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
    );
}

/// The seconds of silence `CaptureSession::gap_frames` writes for `gap`.
fn gap_seconds(gap: Duration) -> f64 {
    CaptureSession::gap_frames(gap) as f64 * FRAME_SIZE as f64 / SAMPLE_RATE
}

/// A device that stops delivering without notification: the watch thread
/// reports the stall, the rebuild restarts, and the gap from the last frame
/// is written as silence.
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
        recv(&notices),
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

/// A stall shorter than `STALL_TIMEOUT` costs no rebuild.
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

/// A stream that delivers nothing from its start is stalled after
/// `STALL_TIMEOUT`, as one that stopped is.
#[test]
fn a_stream_that_never_delivered_is_stalled_too() {
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
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled),
        "a stream that never delivered is stalled too"
    );
    assert!(matches!(
        recv(&notices),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    backend.wait_until_finished();
    session.stop().unwrap();
}

fn resume_delivery_until_a_frame(backend: &SyntheticCaptureBackend) {
    let before = backend.frames_delivered();
    backend.resume_delivery();
    let deadline = Instant::now() + HANG_GUARD;
    while backend.frames_delivered() == before {
        assert!(Instant::now() < deadline, "the restart delivers");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Moves the clock a sample at a time while the watch thread and the
/// rebuild sleep, until `done`; panics after `limit` samples.
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

/// Restarts that start but deliver nothing count as not run: they back
/// off, go on past `RESTART_ATTEMPTS` with `StillRestarting`, and resume
/// on the first that delivers, the gap written once.
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
        recv(&notices),
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

/// The attempt and gap of the next `DeviceResumed`.
fn next_resume(notices: &Receiver<CaptureNotice>) -> (usize, f64) {
    loop {
        if let CaptureNotice::DeviceResumed {
            attempt,
            gap_seconds,
        } = recv(notices)
        {
            return (attempt, gap_seconds);
        }
    }
}

/// A restart that delivers during its wait ends the gap at the wait's last
/// empty sample, not at the start's return.
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
    assert_eq!(next_resume(&notices).1, gap);
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, gap);
}

/// The synthetic backend, whose restarts return `start_takes` on `clock`
/// after their first callback, as a PipeWire start does; with
/// `until_its_stall`, once the producer has delivered up to its stall.
struct SlowRestart {
    inner: SyntheticCaptureBackend,
    clock: Arc<ManualClock>,
    start_takes: Duration,
    until_its_stall: bool,
}

impl CaptureBackend for SlowRestart {
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let stream = self.inner.start(lanes, input_device_uid, sink)?;
        if self.inner.starts() > 1 {
            if self.until_its_stall {
                self.inner.wait_until_finished();
            }
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
}

/// Frames during a restart's `start` end the gap at the reading before the
/// start, so the master does not hold the start's latency twice.
#[test]
fn frames_during_a_slow_restart_end_the_gap_where_they_began() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SlowRestart {
        inner: SyntheticCaptureBackend::new(stalling(1.0, 0.5)),
        clock: clock.clone(),
        start_takes: Duration::from_millis(300),
        until_its_stall: false,
    });
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    // The stall is reported one sample past `STALL_TIMEOUT` after the last
    // frame; the restart starts at once.
    let gap = gap_seconds(CaptureSession::STALL_TIMEOUT + CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(next_resume(&notices).1, gap, "not the start's 0.3 s on top");
    backend.inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, gap);
    assert_eq!(result.statistics.duration, 0.5 + gap + 1.0);
}

/// A 0.3 s restart that delivered 0.1 s counts 0.2 s missed and unfilled on
/// every lane, beside the gap.
#[test]
fn a_slow_restart_counts_the_time_before_its_first_frame() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Arc::new(SlowRestart {
        inner: SyntheticCaptureBackend::new(stalling(1.0, 0.5).restarts_stall_after(0.1)),
        clock: clock.clone(),
        start_takes: Duration::from_millis(300),
        until_its_stall: true,
    });
    let session = in_person_session(directory.path(), backend.clone(), clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    backend.inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let gap = gap_seconds(CaptureSession::STALL_TIMEOUT + CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(next_resume(&notices).1, gap);
    backend.inner.resume_delivery();
    backend.inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, gap);
    assert_eq!(result.statistics.duration, 0.5 + gap + 1.0);
    assert_eq!(result.statistics.filled_seconds, 0.0);
    assert!(
        (result.statistics.unfilled_seconds - 0.2).abs() < 1e-9,
        "{} s",
        result.statistics.unfilled_seconds
    );
}

/// Silence is delivery: zeros that keep arriving are never a stall.
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
/// restarts.
fn backoff_past_the_attempts(extra: usize) -> Vec<Duration> {
    (1..=CaptureSession::RESTART_BACKOFF.len() + extra)
        .map(CaptureSession::restart_backoff)
        .collect()
}

/// The backoff: `RESTART_BACKOFF`, then doubling to
/// `RESTART_BACKOFF_LONGEST` for good.
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

/// Restarts longer than `STALL_TIMEOUT` are no second stall: one resume,
/// no rebuild after.
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
        recv(&notices),
        CaptureNotice::StillRestarting { .. }
    ));
    assert!(matches!(
        recv(&notices),
        CaptureNotice::DeviceResumed { attempt, .. } if attempt == failures + 1
    ));
    backend.wait_until_finished();
    settle();
    // The watch thread may already have ended the warning.
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

/// The gap runs from the last frame the watch thread saw, not from the
/// report, so a coalescing delay leaves no hole.
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
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::InputDeviceGone)
    );
    assert_eq!(
        recv(&notices),
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

/// Waits until `backend` answered `count` probes in all.
fn wait_for_probes(backend: &ChosenOrDefault, count: usize) {
    let deadline = Instant::now() + HANG_GUARD;
    while backend.probed.load(Ordering::SeqCst) < count {
        assert!(Instant::now() < deadline, "probe {count}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// An ask that finds the chosen microphone delivering returns to it, and
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
    let deadline = Instant::now() + HANG_GUARD;
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
        recv(&notices),
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
    // The rebuild's dead time is not on the manual clock, so the master is
    // checked against both starts' frames plus the gap.
    let delivered = backend.inner.frames_delivered();
    let gap = (statistics.gap_seconds * SAMPLE_RATE).round() as usize;
    assert_eq!(
        master_of(&result).frame_count(),
        delivered - delivered % FRAME_SIZE + gap,
        "every whole frame delivered through the return, and the gap"
    );
}

// Echo cancellation in the real processing path

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

/// The backend as the session drives it, noting whether the [`Playback`]
/// gate is held at each `start` and `stop`.
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

/// The session holds the playback gate from before its backend starts,
/// across a rebuild, until the backend has stopped.
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
        let notice = recv(&notices);
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

/// The runs of at least `shortest` exact zeros in `samples`, as (start,
/// length); the test tones touch zero at single samples only.
fn zero_runs(samples: &[f32], shortest: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (index, sample) in samples.iter().chain(std::iter::once(&1.0)).enumerate() {
        match (*sample == 0.0, start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                if index - from >= shortest {
                    runs.push((from, index - from));
                }
                start = None;
            }
            _ => {}
        }
    }
    runs
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every line this test binary logged at `warn` and above, from any thread
/// (a per-test subscriber misses call sites another test registered first).
fn warnings() -> String {
    static CAPTURED: OnceLock<Captured> = OnceLock::new();
    let captured = CAPTURED.get_or_init(|| {
        let captured = Captured::default();
        let writer = captured.clone();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .without_time()
                .finish(),
        )
        .expect("no other subscriber in this test binary");
        captured
    });
    String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned()
}

/// A capture thread that misses 100 ms (filled with zeros) and 2.5 s (past
/// the fill limit, left out): filled and unfilled are counted apart, never
/// as dropped or a gap, and the stop logs both at `warn`.
#[test]
fn missed_cycles_are_written_as_silence_on_every_lane_and_counted() {
    let directory = tempfile::tempdir().unwrap();
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 5.0)
            .miss_at(1.0, 4_800)
            .miss_at(2.0, 120_000),
    ));
    let mut configuration = configuration(CaptureMode::Call, directory.path(), false);
    configuration.echo_cancellation = false;
    let session = CaptureSession::with_backend(
        configuration,
        backend.clone(),
        None,
        1_000,
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    warnings();
    session.start(Uuid::new_v4()).unwrap();
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    let warnings = warnings();
    assert!(
        warnings.contains(
            "the capture's thread missed 2.600 s of audio: 0.100 s written as silence, \
             2.500 s too long to fill and left out"
        ),
        "{warnings}"
    );
    let statistics = &result.statistics;
    assert!(statistics.dropped_frames.is_empty(), "{statistics:?}");
    assert_eq!(statistics.gap_seconds, 0.0);
    assert_eq!(statistics.filled_seconds, 0.1, "the 100 ms filled");
    assert_eq!(statistics.unfilled_seconds, 2.5, "the 2.5 s counted");
    assert!(
        (statistics.duration - 2.5).abs() < 0.02,
        "five seconds less the jump past the limit: {}",
        statistics.duration
    );
    let master = master_of(&result);
    assert_eq!(master.channels.len(), 2);
    assert_eq!(
        master.channels[0].len(),
        master.channels[1].len(),
        "both lanes lost the same span"
    );
    for (lane, samples) in call().iter().zip(&master.channels) {
        assert_eq!(
            zero_runs(samples, 16),
            [(48_000, 4_800)],
            "{lane:?}: zeros exactly where the frames were filled"
        );
    }
}

/// Missed frames count at the rate of the stream that missed them, across
/// a rebuild to 24 kHz.
#[test]
fn missed_cycles_before_a_rebuild_count_at_the_old_streams_rate() {
    let directory = tempfile::tempdir().unwrap();
    let hands_free = CaptureStream {
        sample_rate: 24_000.0,
        input_latency_frames: 240,
        output_latency_frames: 4_800,
        ..CaptureStream::SYNTHETIC
    };
    let backend = Arc::new(SyntheticCaptureBackend::new(
        tones(&call(), 2.0)
            .miss_at(0.5, 2_400)
            .change_device_after(1.0)
            .stream_after_restart(hands_free),
    ));
    let session = CaptureSession::with_backend(
        configuration(CaptureMode::Call, directory.path(), false),
        backend.clone(),
        passthrough(),
        1_000,
        Arc::new(ManualClock::new()),
    )
    .unwrap();
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert!(matches!(
        recv(&notices),
        CaptureNotice::DeviceResumed { .. }
    ));
    backend.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(result.statistics.filled_seconds, 0.05);
}

/// The synthetic backend, whose first `stop()` first counts `late` unfilled
/// frames, as a stalled thread waking before the stop does; the first start
/// counts `early` ones. [`Self::stop_taking`] moves the clock in that stop.
struct WakesBeforeTheStop {
    inner: Arc<SyntheticCaptureBackend>,
    sink: Mutex<Option<Arc<LaneFrameSink>>>,
    early: usize,
    late: AtomicUsize,
    stop_takes: Mutex<Option<(Arc<ManualClock>, Duration)>>,
}

impl WakesBeforeTheStop {
    fn new(inner: &Arc<SyntheticCaptureBackend>, early: usize, late: usize) -> Self {
        Self {
            inner: Arc::clone(inner),
            sink: Mutex::new(None),
            early,
            late: AtomicUsize::new(late),
            stop_takes: Mutex::new(None),
        }
    }

    fn stop_taking(self, clock: &Arc<ManualClock>, by: Duration) -> Self {
        self.stop_takes
            .lock()
            .unwrap()
            .replace((Arc::clone(clock), by));
        self
    }
}

impl CaptureBackend for WakesBeforeTheStop {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let first = self
            .sink
            .lock()
            .unwrap()
            .replace(Arc::clone(&sink))
            .is_none();
        let stream = self.inner.start(lanes, uid, Arc::clone(&sink))?;
        if first {
            sink.write_missed(Missed {
                fill: 0,
                unfilled: self.early,
            });
        }
        Ok(stream)
    }
    fn stop(&self) {
        if let Some((clock, by)) = self.stop_takes.lock().unwrap().take() {
            clock.advance(by);
        }
        let late = self.late.swap(0, Ordering::SeqCst);
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            sink.write_missed(Missed {
                fill: 0,
                unfilled: late,
            });
        }
        self.inner.stop();
    }
    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        self.inner.delivers_continuously(lanes)
    }
}

/// A stalled thread that wakes and counts an unfilled jump before the
/// rebuild's stop: once the gap is written, the late count is taken back
/// up to the gap's length, so the time counts once; an earlier jump stays.
#[test]
fn a_stall_counted_by_its_thread_before_the_rebuilds_stop_counts_once() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let inner = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let backend = Arc::new(WakesBeforeTheStop::new(&inner, 4_800, 96_000));
    let session = in_person_session(directory.path(), backend, clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    let gap_seconds =
        gap_seconds(CaptureSession::STALL_TIMEOUT + CaptureSession::STALL_CHECK_INTERVAL);
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds
        }
    );
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(result.statistics.gap_seconds, gap_seconds);
    assert_eq!(gap_seconds, 2.1);
    assert_eq!(
        result.statistics.unfilled_seconds, 0.1,
        "the jump before the stall, not the stall again"
    );
}

/// As above, but `stop()` comes before the gap is written: the late count
/// stays whole.
#[test]
fn a_late_count_stays_counted_when_a_stop_abandons_the_restart() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let inner = Arc::new(SyntheticCaptureBackend::new(
        stalling(1.0, 0.5).restarts_stall_after(0.0),
    ));
    let backend = Arc::new(WakesBeforeTheStop::new(&inner, 0, 120_000));
    let session = Arc::new(in_person_session(directory.path(), backend, clock.clone()));
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(
        clock.wait_for_sleepers(2),
        "the restart waits for a first frame"
    );
    assert_eq!(inner.starts(), 2);
    let stopping = Arc::clone(&session);
    let result = within_deadline("the stop", move || stopping.stop()).unwrap();
    let statistics = &result.statistics;
    assert_eq!(statistics.device_changes, 0);
    assert_eq!(statistics.gap_seconds, 0.0);
    assert_eq!(statistics.filled_seconds, 0.0);
    assert_eq!(statistics.unfilled_seconds, 2.5, "the late count, whole");
}

/// A 14 s late count against a 12 s stop: the gap writes `MAXIMUM_GAP` and
/// the 4 s it did not write stay unfilled.
#[test]
fn a_late_count_past_the_maximum_gap_keeps_what_the_gap_did_not_write() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let inner = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let backend = Arc::new(
        WakesBeforeTheStop::new(&inner, 0, 14 * 48_000)
            .stop_taking(&clock, Duration::from_secs(12)),
    );
    let session = in_person_session(directory.path(), backend, clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: gap_seconds(CaptureSession::MAXIMUM_GAP)
        }
    );
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, 10.0);
    assert_eq!(result.statistics.unfilled_seconds, 4.0);
}

/// A gap that starts at a device change, not a last frame: a jump counted
/// before the stop stays beside the gap.
#[test]
fn a_late_count_before_a_device_changes_rebuild_stays_counted() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let inner = Arc::new(SyntheticCaptureBackend::new(
        tones(&[AudioLane::Mixed], 1.0).change_device_after(0.5),
    ));
    let backend = Arc::new(
        WakesBeforeTheStop::new(&inner, 0, 120_000).stop_taking(&clock, Duration::from_secs(1)),
    );
    let session = in_person_session(directory.path(), backend, clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceChanged(DeviceChangeReason::DefaultInputChanged)
    );
    assert_eq!(
        recv(&notices),
        CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 1.0
        }
    );
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.gap_seconds, 1.0);
    assert_eq!(
        result.statistics.unfilled_seconds, 2.5,
        "the jump before the change's gap"
    );
}

/// A backend whose first stop hands on what its stream missed before its
/// first frame, as the WASAPI and Core Audio backends do.
struct MissesBeforeItsFirstFrame {
    inner: Arc<SyntheticCaptureBackend>,
    sink: Mutex<Option<Arc<LaneFrameSink>>>,
    before_first: AtomicUsize,
}

impl MissesBeforeItsFirstFrame {
    fn new(inner: &Arc<SyntheticCaptureBackend>, before_first: usize) -> Self {
        Self {
            inner: Arc::clone(inner),
            sink: Mutex::new(None),
            before_first: AtomicUsize::new(before_first),
        }
    }
}

impl CaptureBackend for MissesBeforeItsFirstFrame {
    fn start(
        &self,
        lanes: &[AudioLane],
        uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        self.sink.lock().unwrap().replace(Arc::clone(&sink));
        self.inner.start(lanes, uid, sink)
    }
    fn stop(&self) {
        let before_first = self.before_first.swap(0, Ordering::SeqCst);
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            sink.count_before_first(before_first, None);
        }
        self.inner.stop();
    }
    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        self.inner.delivers_continuously(lanes)
    }
}

/// A first frame 1.5 s late, then a stall whose rebuild is that stop: the
/// gap never covers the time before the first frame, which stays counted.
#[test]
fn a_loss_before_the_first_frame_survives_a_stall_rebuild_at_the_first_stop() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let inner = Arc::new(SyntheticCaptureBackend::new(stalling(1.0, 0.5)));
    let backend = Arc::new(MissesBeforeItsFirstFrame::new(&inner, 72_000));
    let session = in_person_session(directory.path(), backend, clock.clone());
    let notices = session.notices();
    session.start(Uuid::new_v4()).unwrap();
    inner.wait_until_finished();
    drive_into_a_stall(&clock, &notices);
    assert!(matches!(
        recv(&notices),
        CaptureNotice::DeviceResumed { attempt: 1, .. }
    ));
    inner.wait_until_finished();
    let result = session.stop().unwrap();
    assert_eq!(result.statistics.device_changes, 1);
    assert_eq!(result.statistics.gap_seconds, 2.1);
    assert_eq!(
        result.statistics.unfilled_seconds, 1.5,
        "the time before the first frame"
    );
}

/// The same with a device change's rebuild and with the user's stop as the
/// first stop.
#[test]
fn a_loss_before_the_first_frame_counts_at_a_device_changes_rebuild_and_a_plain_stop() {
    for change in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new());
        let options = tones(&[AudioLane::Mixed], 1.0);
        let options = if change {
            options.change_device_after(0.5)
        } else {
            options
        };
        let inner = Arc::new(SyntheticCaptureBackend::new(options));
        let backend = Arc::new(MissesBeforeItsFirstFrame::new(&inner, 72_000));
        let session = in_person_session(directory.path(), backend, clock.clone());
        let notices = session.notices();
        session.start(Uuid::new_v4()).unwrap();
        if change {
            let _ = recv(&notices);
            let _ = recv(&notices);
        }
        inner.wait_until_finished();
        let result = session.stop().unwrap();
        assert_eq!(result.statistics.device_changes, usize::from(change));
        assert_eq!(result.statistics.unfilled_seconds, 1.5, "change {change}");
    }
}
