//! The state machine over a [`CaptureBackend`].
//! Swift: `Sources/StenoAudio/Capture/CaptureSession.swift`.
//!
//! `Idle → Starting → Recording → Stopping → Idle`, or `Failed` when a
//! device stays lost, a write or the close fails, or a sync failed while
//! recording. Owns the sink, the processing
//! thread, the relay, the writer thread and the [`RecordingWriting`]
//! implementation; `stop()` tears them down in order (an in-flight rebuild,
//! the backend, processing, writer, files; see Threads) and returns the
//! [`CaptureResult`]: the finished [`AudioAsset`] (`Caf48kFloat32`,
//! `sidecars_16k` filled, retention `KeepForever` until the caller sets it
//! from `Settings`) with statistics.
//!
//! A device change while recording does not end the recording. The
//! backend reports it through the sink; the session stops the backend and
//! the processing thread, starts the backend again on the devices as they
//! are now (up to [`CaptureSession::RESTART_ATTEMPTS`] times,
//! [`CaptureSession::RESTART_BACKOFF`] apart on the clock), fills the gap
//! with silence through the relay so the master stays on wall time, starts
//! a processing thread built for the new latencies, and keeps the sink, the
//! relay, the writer thread, the writer and the files. The state stays
//! `Recording`; `notices` carries `DeviceChanged` and `DeviceResumed`.
//! Only when every restart fails does the recording end in
//! `Failed(DeviceLost)`. A chosen microphone that is gone fails no restart:
//! the live backends then record the default input and say so in
//! [`CaptureStream::input`], which [`CaptureSession::stream`] hands out
//! (see [`CaptureBackend::start`]).
//!
//! A recording cut short (device loss, a failed write) is finalised and
//! travels in the state: `Failed { error, recording }`. So does the whole
//! recording when the close failed (a full disk while closing) or, with
//! nothing else ending it, a sync failed while recording. `stop()` returns
//! the same result, or fails when the failure left no recording.
//!
//! A writer failure and a device loss finalise on their own threads (the
//! writer failure's and the rebuild's) and hold `Stopping` meanwhile. A
//! `stop()` that finds `Stopping` (one of those finalises, or another
//! `stop()`) waits on a condition variable until the state leaves
//! `Stopping`, then answers from the outcome: the recording `Failed`
//! carries, or `InvalidState` once the state is `Idle`. Swift's actor ran
//! the queued stop right after the finalise; here a `start()` can take the
//! lock first, and the stop then answers `InvalidState` rather than ending
//! the new recording. Only `stop()` waits there; a finaliser never waits
//! for a stopper, so the wait cannot deadlock, and a finaliser that panics
//! leaves `Failed` with no recording rather than `Stopping` for good.
//!
//! # Threads
//!
//! Swift's actor becomes one mutex over the session state. The rule that
//! keeps it deadlock-free: **no thread is joined while the mutex is held**
//! (one bounded exception on Linux, below). The backend's producer, the
//! processing thread and the writer thread all call back into the session
//! (device changes, levels, write errors) and take the mutex to do so;
//! teardown therefore moves the recording out of the state under the
//! lock, releases it, and only then stops the threads.
//! The rebuild runs on its own thread, takes the mutex for each step that
//! touches the state and sleeps outside it on the injected [`Clock`], with
//! a [`Cancel`] token `stop()` raises. Teardown raises that token and then
//! joins the rebuild thread, lock released, before it stops the backend: the
//! rebuild may be inside its own `backend.stop()` with the IOProc and the
//! old processing thread still running, or may just have started the
//! backend again, and the rings are cleared only once both are over. So
//! `stop()` blocks for as long as the rebuild's current step takes: its
//! `backend.stop()` of the old devices and the old processing thread's
//! stop (HAL teardown, up to hundreds of milliseconds, unbounded if the HAL
//! hangs), or a `backend.start()` and its settle (below). When every
//! restart failed, the rebuild finalises the recording itself and first
//! takes its own handle out of the state, so `finish()` never joins the
//! thread it runs on.
//!
//! A stop that cuts a rebuild short loses nothing from the statistics. The
//! join returns the system-lane peak of the processing thread the rebuild
//! stopped, plus any gap silence no `resume` counted. `finish()` reads the
//! ring overruns before it clears the rings, and counts the whole frames
//! still in them (audio a restarted backend delivered before any processing
//! thread ran) as dropped on every lane.
//!
//! The one long hold is deliberate: `start` and the rebuild's
//! `restart_backend` keep the mutex across `backend.start()`: on macOS up
//! to 200 ms while
//! [`NominalSampleRate::settle`](super::NominalSampleRate::settle) waits
//! for the aggregate, on Linux until PipeWire runs the first cycle (1 to
//! 2 s for a Bluetooth sink; none within 3 s fails the start), on Windows
//! until both streams have opened and started (process loopback's
//! activation included; 10 s in all at most, then the start fails). A
//! `stop()` arriving meanwhile queues behind it and then finds a started
//! backend to tear down, instead of racing a half-built one; a backend
//! never calls back into the session from `start`, so the hold cannot
//! deadlock. On Linux a failed start closes the capture's gate and joins
//! the PipeWire thread (2 s at most) with the mutex held, the one join
//! under it; that is safe because no device-change report can begin
//! before `start` took the thread's answer, so nothing that thread does
//! waits on the mutex. It can stall, though: every caller, `state()`
//! included, waits as long as `backend.start()` takes, so a HAL call that
//! hangs there freezes the session's callers with it.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::Utc;
use steno_core::paths::file_url;
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, EchoCanceller, RecordingLayout,
};
use uuid::Uuid;

use super::backend::{CaptureBackend, CaptureStream};
use super::configuration::{
    CaptureConfiguration, CaptureError, CaptureNotice, CaptureResult, CaptureState,
    CaptureStatistics, DeviceChangeReason, LaneLevel, LaneLevels,
};
use crate::aec::SpeexEchoCanceller;
use crate::clock::{Cancel, Clock, SystemClock};
use crate::realtime::{
    FrameRelay, LaneFrameSink, LevelSlot, ProcessingConfiguration, ProcessingThread,
};
use crate::writer::{RecordingWriter, RecordingWriting, WriterThread};
use crate::{FRAME_SIZE, FRAMES_PER_SECOND, SAMPLE_RATE};

/// Opens the files for one recording; [`RecordingWriter::new`] in
/// production, a failure-injecting wrapper in tests.
pub type RecordingWriterFactory = Arc<
    dyn Fn(&RecordingLayout, &[AudioLane], bool) -> Result<Box<dyn RecordingWriting>, CaptureError>
        + Send
        + Sync,
>;

/// A recording session over one backend; see the module doc. Tests run
/// the same pipeline over a synthetic backend and a manual clock through
/// [`CaptureSession::with_backend`].
///
/// ```no_run
/// use steno_audio::{CaptureConfiguration, CaptureMode, CaptureSession};
///
/// let configuration = CaptureConfiguration::new(CaptureMode::Call, "/tmp/steno-audio");
/// let session = CaptureSession::new(configuration)?;
/// session.start(uuid::Uuid::new_v4())?;
/// std::thread::sleep(std::time::Duration::from_secs(2));
/// let result = session.stop()?;
/// println!("{:.1} s at {}", result.statistics.duration, result.asset.url);
/// # Ok::<(), steno_audio::CaptureError>(())
/// ```
pub struct CaptureSession {
    core: Arc<Core>,
}

struct Core {
    configuration: CaptureConfiguration,
    writer_headroom_frames: usize,
    backend: Arc<dyn CaptureBackend>,
    clock: Arc<dyn Clock>,
    make_writer: RecordingWriterFactory,
    inner: Mutex<Inner>,
    /// Notified on every state change; `stop()` waits on it while another
    /// thread's finalise holds `Stopping`.
    state_changed: Condvar,
}

struct Inner {
    state: CaptureState,
    state_subscribers: Vec<Sender<CaptureState>>,
    level_subscribers: Vec<Sender<LaneLevels>>,
    notice_subscribers: Vec<Sender<CaptureNotice>>,
    latest_levels: Option<LaneLevels>,
    /// The one canceller every recording of the session shares, held here
    /// while no processing thread runs it.
    echo_canceller: Option<Box<dyn EchoCanceller>>,
    active: Option<Active>,
    /// Bumped per rebuild and never reset, so a rebuild thread that wakes
    /// after a stop and a new start does nothing.
    rebuild_generation: usize,
    /// Bumped by every `start` and never reset, so a `stop()` that waited
    /// and a writer failure that arrives late act on their own recording
    /// only, never on one started meanwhile.
    recordings_started: usize,
    /// Unit tests only: taken by the next `stop()` that finds `Stopping`
    /// and run once it has noted `recordings_started`, with the lock
    /// released, so a test can end the finalise and run a `start()` before
    /// that `stop()` takes the lock back. The tests built on it prove the
    /// guard after the wait, not the condition-variable wait itself.
    #[cfg(test)]
    before_stop_waits: Option<Box<dyn FnOnce() + Send>>,
}

struct Active {
    meeting_id: Uuid,
    stream: CaptureStream,
    sink: Arc<LaneFrameSink>,
    relay: Arc<FrameRelay>,
    /// `None` only between a rebuild taking the old thread and installing
    /// the new one.
    processing: Option<ProcessingThread>,
    writer_thread: Option<WriterThread>,
    levels: Arc<LevelSlot>,
    ended_on_device_loss: bool,
    /// Rebuilds that succeeded.
    device_changes: usize,
    /// Silence written across those rebuilds.
    gap_seconds: f64,
    /// The loudest system-lane sample over every processing thread
    /// replaced so far; `finish()` takes the maximum with the current one.
    system_peak_so_far: f32,
    /// The rebuild in flight, so `stop()` can abandon it and wait for it.
    rebuild: Option<Rebuild>,
    /// A change reported while that rebuild ran (the rebuilt backend's
    /// listeners are live before the gap is written); `resume` starts the
    /// next rebuild from it instead of losing it.
    pending_change: Option<DeviceChangeReason>,
}

/// A rebuild thread and the token that abandons it.
struct Rebuild {
    cancel: Cancel,
    /// Returns the silence frames the rebuild wrote that no `resume`
    /// accounted, because a stop overtook it, and the system-lane peak of
    /// the processing thread it stopped, which `finish()` then folds in.
    thread: JoinHandle<(usize, f32)>,
}

enum Restart {
    Started(CaptureStream, usize),
    Abandoned,
    Exhausted,
}

impl CaptureSession {
    /// The waits before the second, third and fourth restart after a device
    /// change: a Bluetooth device is gone for one to two seconds while it
    /// changes profile; a device replugged by hand takes longer and is a
    /// loss the user can see and restart from.
    pub const RESTART_BACKOFF: [Duration; 3] = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
    ];
    /// Restarts tried before the recording ends in `DeviceLost`: one more
    /// than the waits between them.
    pub const RESTART_ATTEMPTS: usize = Self::RESTART_BACKOFF.len() + 1;
    /// The most silence written for one gap; a longer outage leaves the
    /// master that much short of wall time rather than filling minutes of
    /// zeros.
    pub const MAXIMUM_GAP: Duration = Duration::from_secs(10);
    /// Frames the writer may fall behind the processing thread before
    /// frames are dropped and counted: 2000 (20 s) by default, so a disk
    /// that stalls for seconds (a slow sync, a sleeping external drive)
    /// loses nothing. The rings round up to 2^20 samples, 4 MiB per written
    /// channel, allocated at start. Rust only: Swift's relay held 2 s.
    pub const DEFAULT_WRITER_HEADROOM_FRAMES: usize = 20 * FRAMES_PER_SECOND;

    /// The production session: the live backend, Speex when the
    /// configuration cancels echo, the wall clock.
    pub fn new(configuration: CaptureConfiguration) -> Result<Self, CaptureError> {
        Self::with_backend(
            configuration,
            Arc::new(super::live::LiveCaptureBackend::new()),
            None,
            Self::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(SystemClock::new()),
        )
    }

    /// `echo_canceller` `None` in `Call` with `echo_cancellation` on means
    /// [`SpeexEchoCanceller`] with the 200 ms tail; `InPerson` never
    /// cancels. `writer_headroom_frames` is the relay depth between
    /// processing and file I/O; a test that feeds audio faster than real
    /// time raises it so a slow disk in a debug build is not mistaken for
    /// a drop. `clock` paces the restart backoff and measures the gap after
    /// a device change; tests pass a `ManualClock`.
    pub fn with_backend(
        configuration: CaptureConfiguration,
        backend: Arc<dyn CaptureBackend>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
        writer_headroom_frames: usize,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, CaptureError> {
        Self::with_writer_factory(
            configuration,
            backend,
            echo_canceller,
            writer_headroom_frames,
            clock,
            Arc::new(|layout, lanes, keep_raw| {
                Ok(Box::new(RecordingWriter::new(layout, lanes, keep_raw)?)
                    as Box<dyn RecordingWriting>)
            }),
        )
    }

    /// As [`Self::with_backend`], with the file writer injected too; tests wrap
    /// the real one to fail writes the way a full disk does.
    pub fn with_writer_factory(
        configuration: CaptureConfiguration,
        backend: Arc<dyn CaptureBackend>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
        writer_headroom_frames: usize,
        clock: Arc<dyn Clock>,
        make_writer: RecordingWriterFactory,
    ) -> Result<Self, CaptureError> {
        let echo_canceller = if configuration.uses_echo_cancellation() {
            match echo_canceller {
                Some(canceller) => Some(canceller),
                None => Some(Box::new(
                    SpeexEchoCanceller::new(SAMPLE_RATE, FRAME_SIZE)
                        .map_err(|e| CaptureError::BackendFailed(e.to_string()))?,
                ) as Box<dyn EchoCanceller>),
            }
        } else {
            None
        };
        Ok(Self {
            core: Arc::new(Core {
                configuration,
                writer_headroom_frames,
                backend,
                clock,
                make_writer,
                inner: Mutex::new(Inner {
                    state: CaptureState::Idle,
                    state_subscribers: Vec::new(),
                    level_subscribers: Vec::new(),
                    notice_subscribers: Vec::new(),
                    latest_levels: None,
                    echo_canceller,
                    active: None,
                    rebuild_generation: 0,
                    recordings_started: 0,
                    #[cfg(test)]
                    before_stop_waits: None,
                }),
                state_changed: Condvar::new(),
            }),
        })
    }

    /// The configuration given at construction.
    #[must_use]
    pub fn configuration(&self) -> &CaptureConfiguration {
        &self.core.configuration
    }

    /// The relay depth between processing and file I/O, in frames.
    #[must_use]
    pub fn writer_headroom_frames(&self) -> usize {
        self.core.writer_headroom_frames
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> CaptureState {
        self.core.lock().state.clone()
    }

    /// Every state change from now on, starting with the current state.
    #[must_use]
    pub fn states(&self) -> Receiver<CaptureState> {
        let (sender, receiver) = channel();
        let mut inner = self.core.lock();
        let _ = sender.send(inner.state.clone());
        inner.state_subscribers.push(sender);
        receiver
    }

    /// Lane levels at 10 Hz while recording, starting with the latest.
    #[must_use]
    pub fn levels(&self) -> Receiver<LaneLevels> {
        let (sender, receiver) = channel();
        let mut inner = self.core.lock();
        if let Some(latest) = inner.latest_levels {
            let _ = sender.send(latest);
        }
        inner.level_subscribers.push(sender);
        receiver
    }

    /// Device changes from now on, while the state stays `Recording`.
    #[must_use]
    pub fn notices(&self) -> Receiver<CaptureNotice> {
        let (sender, receiver) = channel();
        self.core.lock().notice_subscribers.push(sender);
        receiver
    }

    /// The stream the backend opened for the current recording, the rebuilt
    /// backend's after a device change; `None` while not recording.
    #[must_use]
    pub fn stream(&self) -> Option<CaptureStream> {
        self.core
            .lock()
            .active
            .as_ref()
            .map(|active| active.stream.clone())
    }

    /// Starts a recording for `meeting_id` in its own folder of the configured
    /// directory; `InvalidState` while one is starting, recording or stopping.
    pub fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError> {
        self.core.start(meeting_id)
    }

    /// Ends the recording and returns it. A rebuild in flight is abandoned.
    /// After `Failed` returns the finalised partial recording the state
    /// carries, or fails when the failure left no recording. While a writer
    /// failure or a device loss is finalising, waits for it and answers from
    /// its outcome; `InvalidState` when a `start()` came in first. A write or
    /// close that fails during the teardown leaves the state `Failed` with the
    /// recording that is returned, and so does a sync that failed while
    /// recording: the recording is whole, part of it may not have reached the
    /// disk, and a sync failure is reported only when nothing else ended the
    /// recording. Fails with `WriterFailed`, and leaves the state `Failed`,
    /// when the master is gone from disk or the writer thread died.
    pub fn stop(&self) -> Result<CaptureResult, CaptureError> {
        self.core.stop()
    }

    /// The sink's handler, on the session. Ignored unless recording; during
    /// a rebuild the reason is kept for `resume`; otherwise the notice goes
    /// out and the rebuild runs on its own thread so `stop()` can
    /// interleave at its sleeps. Public so a test can report a change while
    /// idle, after a stop or during a rebuild; production reaches it
    /// through the sink alone.
    pub fn device_changed(&self, reason: DeviceChangeReason) {
        self.core.device_changed(reason);
    }

    /// The far-end delay for the latencies the backend reports. The mic
    /// hears the tap's signal after the output path (output latency plus
    /// safety offset), the room and the input path (input latency plus
    /// safety offset), so the far-end is delayed by the two device paths in
    /// full and the Speex tail (200 ms) is left for the room and for what
    /// the HAL under-reports. Below one processing frame the tail absorbs
    /// the offset as well; over-delaying is the one thing the MDF filter
    /// cannot recover from, so nothing is rounded up.
    #[must_use]
    pub fn far_end_delay_frames(
        input_latency_frames: usize,
        output_latency_frames: usize,
    ) -> usize {
        let total = input_latency_frames + output_latency_frames;
        if total >= FRAME_SIZE { total } else { 0 }
    }

    /// Whole relay frames for a gap: 48 000 samples a second in frames of
    /// [`FRAME_SIZE`], rounded down.
    #[must_use]
    pub fn gap_frames(gap: Duration) -> usize {
        // A capped, positive duration in samples.
        let samples = (gap.as_secs_f64() * SAMPLE_RATE).floor() as usize;
        samples / FRAME_SIZE
    }
}

impl Drop for CaptureSession {
    /// A session dropped mid-recording still stops its threads and closes
    /// its files.
    fn drop(&mut self) {
        let recording = matches!(self.core.lock().state, CaptureState::Recording { .. });
        if recording {
            let _ = self.core.stop();
        }
    }
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_state(&self, inner: &mut Inner, state: &CaptureState) {
        inner.state = state.clone();
        inner
            .state_subscribers
            .retain(|s| s.send(state.clone()).is_ok());
        self.state_changed.notify_all();
    }

    fn publish(&self, levels: LaneLevels) {
        let mut inner = self.lock();
        inner.latest_levels = Some(levels);
        inner.level_subscribers.retain(|s| s.send(levels).is_ok());
    }

    fn emit(inner: &mut Inner, notice: CaptureNotice) {
        inner.notice_subscribers.retain(|s| s.send(notice).is_ok());
    }

    fn far_end_delay_frames(&self, stream: &CaptureStream) -> usize {
        if !self.configuration.uses_echo_cancellation() {
            return 0;
        }
        CaptureSession::far_end_delay_frames(
            stream.input_latency_frames,
            stream.output_latency_frames,
        )
    }

    /// Whether the relay and the writer carry the raw microphone channel.
    fn keep_raw(&self) -> bool {
        self.configuration.keep_raw_mic_lane && self.configuration.lanes().contains(&AudioLane::Mic)
    }

    /// The seconds `frames` relay frames of silence last, exact for any
    /// gap length.
    fn seconds(frames: usize) -> f64 {
        (frames * FRAME_SIZE) as f64 / SAMPLE_RATE
    }

    fn make_processing_thread(
        &self,
        sink: &Arc<LaneFrameSink>,
        relay: &Arc<FrameRelay>,
        stream: &CaptureStream,
        levels: Option<Arc<LevelSlot>>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
    ) -> ProcessingThread {
        let mut configuration =
            ProcessingConfiguration::new(&self.configuration.lanes(), echo_canceller);
        configuration.far_end_delay_frames = self.far_end_delay_frames(stream);
        configuration.keep_raw_mic = self.keep_raw();
        ProcessingThread::new(Arc::clone(sink), Arc::clone(relay), configuration, levels)
    }

    #[allow(clippy::too_many_lines)]
    fn start(self: &Arc<Self>, meeting_id: Uuid) -> Result<(), CaptureError> {
        let mut inner = self.lock();
        match inner.state {
            CaptureState::Idle | CaptureState::Failed { .. } => {}
            ref other => {
                return Err(CaptureError::InvalidState(format!(
                    "start while {}",
                    other.kind()
                )));
            }
        }
        // `Starting` carries no recording: a failed start must never hand
        // out the previous meeting's files.
        self.set_state(&mut inner, &CaptureState::Starting);
        inner.recordings_started += 1;
        let recording = inner.recordings_started;
        // A new recording, possibly on other devices, starts from a cold filter.
        if let Some(canceller) = inner.echo_canceller.as_mut() {
            canceller.reset();
        }
        let lanes = self.configuration.lanes();
        let layout = RecordingLayout::new(&self.configuration.output_directory, meeting_id);
        let keep_raw = self.keep_raw();

        let writer = match (self.make_writer)(&layout, &lanes, keep_raw) {
            Ok(writer) => writer,
            Err(error) => {
                let failure = as_writer_failure(error);
                self.set_state(
                    &mut inner,
                    &CaptureState::Failed {
                        error: failure.clone(),
                        recording: None,
                    },
                );
                return Err(failure);
            }
        };

        let weak: Weak<Core> = Arc::downgrade(self);
        let sink = Arc::new(LaneFrameSink::with_handler(
            &lanes,
            SAMPLE_RATE,
            2.0,
            Box::new(move |reason| {
                if let Some(core) = weak.upgrade() {
                    core.device_changed_in(recording, reason);
                }
            }),
        ));
        let stream = match self.backend.start(
            &lanes,
            self.configuration.input_device_uid.as_deref(),
            Arc::clone(&sink),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                let mut writer = writer;
                let _ = writer.finish();
                let _ = std::fs::remove_dir_all(&layout.directory);
                self.set_state(
                    &mut inner,
                    &CaptureState::Failed {
                        error: error.clone(),
                        recording: None,
                    },
                );
                return Err(error);
            }
        };

        let relay = Arc::new(FrameRelay::new(
            lanes.len() + usize::from(keep_raw),
            FRAME_SIZE,
            self.writer_headroom_frames,
        ));
        let canceller = inner.echo_canceller.take();
        let mut processing = self.make_processing_thread(&sink, &relay, &stream, None, canceller);
        let levels = Arc::clone(processing.levels());
        let on_levels: Weak<Core> = Arc::downgrade(self);
        let on_error: Weak<Core> = Arc::downgrade(self);
        let mut writer_thread = WriterThread::new(
            Arc::clone(&relay),
            writer,
            Arc::clone(&levels),
            lanes.len(),
            keep_raw,
            Box::new(move |levels| {
                if let Some(core) = on_levels.upgrade() {
                    core.publish(levels);
                }
            }),
            Box::new(move |error| {
                // The writer thread reports this; finalising joins that
                // thread, so it runs on its own, as Swift's `Task` did.
                if let Some(core) = on_error.upgrade() {
                    std::thread::Builder::new()
                        .name("steno-wfail".into())
                        .spawn(move || core.writer_failed(&error, recording))
                        .expect("spawn writer failure thread");
                }
            }),
        );
        writer_thread.start();
        processing.start();

        inner.active = Some(Active {
            meeting_id,
            stream,
            sink,
            relay,
            processing: Some(processing),
            writer_thread: Some(writer_thread),
            levels,
            ended_on_device_loss: false,
            device_changes: 0,
            gap_seconds: 0.0,
            system_peak_so_far: 0.0,
            rebuild: None,
            pending_change: None,
        });
        self.set_state(
            &mut inner,
            &CaptureState::Recording {
                started_at: Utc::now(),
            },
        );
        Ok(())
    }

    fn stop(&self) -> Result<CaptureResult, CaptureError> {
        {
            // Another thread's finalise (see the module doc) is waited out.
            // The loop re-checks after every wakeup: a spurious one, or a
            // `Stopping` left and entered again before this thread got the
            // lock back. A poisoned lock is read as it is, so a panic
            // elsewhere does not end the wait early.
            let mut inner = self.lock();
            let recording = inner.recordings_started;
            #[cfg(test)]
            if matches!(inner.state, CaptureState::Stopping)
                && let Some(hook) = inner.before_stop_waits.take()
            {
                drop(inner);
                hook();
                inner = self.lock();
            }
            while matches!(inner.state, CaptureState::Stopping) {
                inner = self
                    .state_changed
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            // A `start()` took the lock first: the recording this stop was
            // for has ended, and the new one is not this caller's to stop.
            if inner.recordings_started != recording {
                return Err(CaptureError::InvalidState(
                    "stop after another recording started".into(),
                ));
            }
            match &inner.state {
                CaptureState::Recording { .. } => {}
                CaptureState::Failed { recording, .. } => {
                    return match recording {
                        Some(recording) => Ok((**recording).clone()),
                        None => Err(CaptureError::InvalidState(
                            "stop after a failure that left no recording".into(),
                        )),
                    };
                }
                other => {
                    return Err(CaptureError::InvalidState(format!(
                        "stop while {}",
                        other.kind()
                    )));
                }
            }
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let unwinding = Unwinding::arm(
            self,
            CaptureError::BackendFailed("the teardown panicked".into()),
        );
        let finished = self.finish();
        // Closing the files can fail on a full disk; the master is still
        // readable to its last frame, so the result comes back and the
        // state carries the failure instead of `stop()` throwing it away.
        let state = match &finished {
            Ok((_, None)) => CaptureState::Idle,
            Ok((result, Some(error))) => CaptureState::Failed {
                error: error.clone(),
                recording: Some(Box::new(result.clone())),
            },
            Err(error) => CaptureState::Failed {
                error: error.clone(),
                recording: None,
            },
        };
        self.set_state(&mut self.lock(), &state);
        unwinding.disarm();
        finished.map(|(result, _)| result)
    }

    /// Rebuild abandoned and joined, backend off, rings drained, relay
    /// drained, files closed, asset built. The asset is built even when
    /// closing the files fails (its paths are fixed at start and the
    /// duration is what the master holds); the failure comes back beside
    /// it: a failed write or close, else the device loss that ended the
    /// recording, else a sync that failed while recording.
    /// `WriterFailed` with no asset when there is nothing to hand out:
    /// the writer thread died and took the writer with it, or the master is
    /// gone from disk (its folder deleted while recording; an unlinked file
    /// still writes and closes without an error). The caller has set the
    /// state to `Stopping`; the threads are stopped with the lock released
    /// (see the module doc).
    fn finish(&self) -> Result<(CaptureResult, Option<CaptureError>), CaptureError> {
        let mut active = self
            .lock()
            .active
            .take()
            .ok_or_else(|| CaptureError::InvalidState("nothing to finish".into()))?;
        if let Some(rebuild) = active.rebuild.take() {
            rebuild.cancel.cancel();
            // With `active` gone every step of the rebuild gives up; what
            // it wrote of a gap before that is in the master, and the peak
            // of the processing thread it stopped comes back with it. The
            // join must come before the processing thread's stop, the
            // writer's and `clear()`; before or after `backend.stop()` is
            // the same, since a backend stop while the rebuild's own runs
            // finds the backend's state already taken and returns at once.
            let (unaccounted, peak) = rebuild.thread.join().unwrap_or((0, 0.0));
            active.gap_seconds += Self::seconds(unaccounted);
            active.system_peak_so_far = active.system_peak_so_far.max(peak);
        }
        self.backend.stop();
        let mut system_peak = active.system_peak_so_far;
        if let Some(mut processing) = active.processing.take() {
            processing.stop();
            system_peak = system_peak.max(processing.system_peak());
            self.lock().echo_canceller = processing.take_echo_canceller();
        }
        let writer_lost =
            || CaptureError::WriterFailed("the writer thread ended without its files".into());
        let mut writer_thread = active.writer_thread.take().ok_or_else(writer_lost)?;
        writer_thread.stop();
        // A write that failed during this drain went to `writer_failed`,
        // which ignores it once the state is `Stopping`; it comes back
        // beside the asset instead, as a failed close does. A sync that
        // failed while recording is kept apart, behind both.
        let write_failure = writer_thread.take_error();
        let sync_failure = writer_thread.take_sync_error();
        let mut writer = writer_thread.take_writer().ok_or_else(writer_lost)?;
        // Read before `clear()`, which zeroes the ring overrun counts. Whole
        // frames still in the rings never reached the relay: a restarted
        // backend delivered them after a stop overtook its rebuild, with no
        // processing thread running yet. They count as dropped.
        let ring_drops = active.sink.dropped_samples();
        let undrained = active.sink.available_to_read() / FRAME_SIZE;
        active.sink.clear();
        let closing = writer.finish().err();
        let failure = write_failure
            .or(closing)
            .map(as_writer_failure)
            .or_else(|| {
                active
                    .ended_on_device_loss
                    .then_some(CaptureError::DeviceLost)
            })
            .or(sync_failure);
        let files = writer.files();
        if matches!(files.master.try_exists(), Ok(false)) {
            return Err(CaptureError::WriterFailed(format!(
                "{} is gone",
                files.master.display()
            )));
        }
        let lanes = self.configuration.lanes();
        let mut dropped: BTreeMap<AudioLane, usize> = BTreeMap::new();
        for (lane, samples) in ring_drops {
            *dropped.entry(lane).or_default() += samples / FRAME_SIZE;
        }
        if undrained > 0 {
            for lane in &lanes {
                *dropped.entry(*lane).or_default() += undrained;
            }
        }
        for (index, frames) in active.relay.dropped_frames().into_iter().enumerate() {
            if index < lanes.len() && frames > 0 {
                *dropped.entry(lanes[index]).or_default() += frames;
            }
        }
        let statistics = CaptureStatistics {
            duration: files.duration,
            dropped_frames: dropped,
            system_lane_silent: lanes.contains(&AudioLane::System)
                && system_peak < LaneLevel::SILENT_PEAK_LINEAR,
            ended_on_device_loss: active.ended_on_device_loss,
            device_changes: active.device_changes,
            gap_seconds: active.gap_seconds,
        };
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: active.meeting_id,
            url: file_url(&files.master, false),
            format: AudioFormat::Caf48kFloat32,
            lanes: lanes.clone(),
            sidecars_16k: files
                .sidecars_16k
                .iter()
                .map(|(lane, path)| (*lane, file_url(path, false)))
                .collect(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        };
        Ok((CaptureResult { asset, statistics }, failure))
    }

    // Device changes

    fn device_changed(self: &Arc<Self>, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        self.begin_rebuild(&mut inner, reason);
    }

    /// [`Self::device_changed`] for a report from the backend of
    /// `recording`: one an older recording's backend left in its handler
    /// past that recording's stop reaches nothing.
    fn device_changed_in(self: &Arc<Self>, recording: usize, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        if inner.recordings_started == recording {
            self.begin_rebuild(&mut inner, reason);
        }
    }

    /// [`Self::device_changed`] under the caller's guard.
    fn begin_rebuild(self: &Arc<Self>, inner: &mut Inner, reason: DeviceChangeReason) {
        if !matches!(inner.state, CaptureState::Recording { .. }) {
            return;
        }
        let generation = inner.rebuild_generation + 1;
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        if active.rebuild.is_some() {
            active.pending_change = Some(reason);
            return;
        }
        let cancel = Cancel::new();
        let core = Arc::clone(self);
        let token = cancel.clone();
        // Spawned under the lock so the handle is in place before any
        // `finish()` can look for it; the thread's first step waits for
        // the lock.
        let thread = std::thread::Builder::new()
            .name("steno-rebuild".into())
            .spawn(move || core.rebuild(generation, &token))
            .expect("spawn rebuild thread");
        active.rebuild = Some(Rebuild { cancel, thread });
        inner.rebuild_generation = generation;
        Self::emit(inner, CaptureNotice::DeviceChanged(reason));
    }

    fn still_rebuilding(inner: &Inner, generation: usize) -> bool {
        matches!(inner.state, CaptureState::Recording { .. })
            && inner.active.as_ref().is_some_and(|a| a.rebuild.is_some())
            && inner.rebuild_generation == generation
    }

    /// Old backend and processing thread off, then `start` again with
    /// backoff; the gap from the moment the old backend was told to stop
    /// is written as silence before the new processing thread starts. The
    /// sink, the relay, the writer thread and the files stay. Nothing here
    /// runs on a real-time thread. Returns the silence frames written that
    /// `resume` did not account because a stop came first, and the old
    /// processing thread's system-lane peak for a `finish()` that took the
    /// recording before this thread could fold it in.
    fn rebuild(self: &Arc<Self>, generation: usize, cancel: &Cancel) -> (usize, f32) {
        // The stopwatch runs from before the teardown: the HAL calls in
        // `stop()` take tens to hundreds of milliseconds during a device
        // transition, and that is dead time in the master too.
        let started = self.clock.now();
        let (sink, relay, processing) = {
            let mut inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return (0, 0.0);
            }
            let Some(active) = inner.active.as_mut() else {
                return (0, 0.0);
            };
            (
                Arc::clone(&active.sink),
                Arc::clone(&active.relay),
                active.processing.take(),
            )
        };
        // Whatever whole frames the rings hold are the old device's last
        // audio; the processing thread's stop drains them into the relay.
        self.backend.stop();
        let mut canceller = None;
        let mut peak = 0.0f32;
        if let Some(mut processing) = processing {
            processing.stop();
            peak = processing.system_peak();
            canceller = processing.take_echo_canceller();
        }
        // New devices mean a new echo path: the filter starts cold, as at start.
        if let Some(canceller) = canceller.as_mut() {
            canceller.reset();
        }
        {
            let mut inner = self.lock();
            inner.echo_canceller = canceller;
            // Gone when `finish()` took the recording meanwhile; the peak
            // then reaches it through this thread's return value.
            if let Some(active) = inner.active.as_mut() {
                active.system_peak_so_far = active.system_peak_so_far.max(peak);
            }
        }
        // The old backend's `stop()` lets no new report through, so the
        // latch can open now: a report from the rebuilt backend before the
        // gap is written reaches `device_changed`, which keeps it for
        // `resume`. A report the old backend left in the handler reaches
        // `device_changed` late and costs one more rebuild (see the
        // PipeWire backend's module doc).
        sink.rearm_device_change();
        let unaccounted = match self.restart_backend(&sink, generation, cancel) {
            Restart::Started(stream, attempt) => {
                // The gap grows through every failed attempt and is written
                // once, in full, when a start succeeds.
                let elapsed = self.clock.now().saturating_sub(started);
                let gap_frames =
                    CaptureSession::gap_frames(elapsed.min(CaptureSession::MAXIMUM_GAP));
                let written = self.write_silence(gap_frames, &relay, generation, cancel);
                if written == gap_frames
                    && self.relay_has_room(&sink, &relay, generation, cancel)
                    && self.resume(stream, attempt, gap_frames, &sink, &relay, generation)
                {
                    0
                } else {
                    written
                }
            }
            Restart::Abandoned => 0,
            Restart::Exhausted => {
                self.device_lost();
                0
            }
        };
        (unaccounted, peak)
    }

    /// `start` again, `RESTART_BACKOFF` apart on the clock: `Started` with
    /// the attempt that succeeded, `Exhausted` after `RESTART_ATTEMPTS`
    /// failures, `Abandoned` when `stop()` cancelled a sleep or the
    /// recording is gone.
    fn restart_backend(
        &self,
        sink: &Arc<LaneFrameSink>,
        generation: usize,
        cancel: &Cancel,
    ) -> Restart {
        for attempt in 1..=CaptureSession::RESTART_ATTEMPTS {
            let result = {
                let inner = self.lock();
                if !Self::still_rebuilding(&inner, generation) {
                    return Restart::Abandoned;
                }
                self.backend.start(
                    &self.configuration.lanes(),
                    self.configuration.input_device_uid.as_deref(),
                    Arc::clone(sink),
                )
            };
            if let Ok(stream) = result {
                return Restart::Started(stream, attempt);
            }
            if attempt >= CaptureSession::RESTART_ATTEMPTS {
                return Restart::Exhausted;
            }
            if !self
                .clock
                .sleep(CaptureSession::RESTART_BACKOFF[attempt - 1], cancel)
            {
                return Restart::Abandoned;
            }
            if !Self::still_rebuilding(&self.lock(), generation) {
                return Restart::Abandoned;
            }
        }
        Restart::Exhausted
    }

    /// The new processing thread on the kept sink and relay, built for
    /// `stream`'s latencies and publishing into the shared `LevelSlot`; the
    /// statistics and the notice follow. A change reported during the
    /// rebuild starts the next one. `false` when a stop came first.
    fn resume(
        self: &Arc<Self>,
        stream: CaptureStream,
        attempt: usize,
        gap_frames: usize,
        sink: &Arc<LaneFrameSink>,
        relay: &Arc<FrameRelay>,
        generation: usize,
    ) -> bool {
        let mut inner = self.lock();
        if !Self::still_rebuilding(&inner, generation) {
            return false;
        }
        let canceller = inner.echo_canceller.take();
        let Some(active) = inner.active.as_mut() else {
            return false;
        };
        let gap_seconds = Self::seconds(gap_frames);
        let mut processing = self.make_processing_thread(
            sink,
            relay,
            &stream,
            Some(Arc::clone(&active.levels)),
            canceller,
        );
        processing.start();
        let pending = active.pending_change.take();
        active.stream = stream;
        active.processing = Some(processing);
        active.device_changes += 1;
        active.gap_seconds += gap_seconds;
        active.rebuild = None;
        Self::emit(
            &mut inner,
            CaptureNotice::DeviceResumed {
                attempt,
                gap_seconds,
            },
        );
        // Must stay under this guard: released and taken again, a stop and a
        // new start could come in between and hand this recording's change
        // to the next one. No test reaches that window, so this comment is
        // what keeps the call here.
        if let Some(pending) = pending {
            self.begin_rebuild(&mut inner, pending);
        }
        true
    }

    /// Zeros in every written channel for `frames` relay frames, through
    /// the relay the writer thread keeps draining. The rings under the sink
    /// are not touched: they hold two seconds and nothing drains them while
    /// the processing thread is stopped, so a longer gap would silently
    /// shrink into `dropped_samples`. A full relay (a long gap, or a writer
    /// still behind the old producer) is waited out in 5 ms steps on the
    /// clock; `has_room` is asked first because a refused `begin_frame`
    /// counts as a dropped frame. Returns the frames written, fewer than
    /// `frames` when the rebuild was abandoned meanwhile.
    fn write_silence(
        &self,
        frames: usize,
        relay: &FrameRelay,
        generation: usize,
        cancel: &Cancel,
    ) -> usize {
        if frames == 0 {
            return 0;
        }
        let zeros = vec![0.0f32; relay.frame_size()];
        let mut written = 0;
        while written < frames {
            if !Self::still_rebuilding(&self.lock(), generation) {
                break;
            }
            if relay.has_room() && relay.begin_frame() {
                for channel in 0..relay.channels() {
                    relay.write(channel, &zeros);
                }
                relay.end_frame();
                written += 1;
            } else if !self.clock.sleep(Duration::from_millis(5), cancel) {
                break;
            }
        }
        written
    }

    /// Waits, in 5 ms steps on the clock, until the relay has room for the
    /// whole frames the rings collected while the gap was written (capped
    /// at the relay's capacity, re-read on every step): the new processing
    /// thread pushes them at once, and a relay still full of silence would
    /// refuse and count them. Returns `false` when the rebuild was
    /// abandoned meanwhile.
    fn relay_has_room(
        &self,
        sink: &LaneFrameSink,
        relay: &FrameRelay,
        generation: usize,
        cancel: &Cancel,
    ) -> bool {
        let backlog = || (sink.available_to_read() / FRAME_SIZE).min(relay.capacity_frames());
        while relay
            .capacity_frames()
            .saturating_sub(relay.available_frames())
            < backlog()
        {
            if !Self::still_rebuilding(&self.lock(), generation) {
                return false;
            }
            if !self.clock.sleep(Duration::from_millis(5), cancel) {
                return false;
            }
        }
        true
    }

    /// Every restart failed: the recording ends, finalised and carried in
    /// `Failed(DeviceLost)`.
    fn device_lost(&self) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. }) {
                return;
            }
            let Some(active) = inner.active.as_mut() else {
                return;
            };
            active.ended_on_device_loss = true;
            // This runs inside the rebuild thread; `finish()` must not
            // cancel or join it, and it is over anyway.
            active.rebuild = None;
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let unwinding = Unwinding::arm(self, CaptureError::DeviceLost);
        let state = match self.finish() {
            Ok((result, failure)) => CaptureState::Failed {
                error: failure.unwrap_or(CaptureError::DeviceLost),
                recording: Some(Box::new(result)),
            },
            Err(error) => CaptureState::Failed {
                error,
                recording: None,
            },
        };
        self.set_state(&mut self.lock(), &state);
        unwinding.disarm();
    }

    /// The writer thread of the start numbered `recording` failed; a
    /// failure that arrives after that recording ended is ignored.
    fn writer_failed(&self, error: &CaptureError, recording: usize) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. })
                || inner.recordings_started != recording
            {
                return;
            }
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let failure = as_writer_failure(error.clone());
        let unwinding = Unwinding::arm(self, failure.clone());
        let result = self.finish().ok().map(|(result, _)| Box::new(result));
        let mut inner = self.lock();
        self.set_state(
            &mut inner,
            &CaptureState::Failed {
                error: failure,
                recording: result,
            },
        );
        unwinding.disarm();
    }
}

/// `error` as a `WriterFailed`: one that already is passes through, so its
/// `Display` says "writing the recording failed" once.
fn as_writer_failure(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::WriterFailed(_) => error,
        other => CaptureError::WriterFailed(other.to_string()),
    }
}

/// Armed while a finalise holds `Stopping`. If the finalise panics, the
/// drop during the unwind sets `Failed { error, recording: None }` and
/// wakes the waiting `stop()`s, which would otherwise wait for good; the
/// next `start()` then works. A finalise that returns disarms it.
struct Unwinding<'a> {
    core: &'a Core,
    error: Option<CaptureError>,
}

impl<'a> Unwinding<'a> {
    fn arm(core: &'a Core, error: CaptureError) -> Self {
        Self {
            core,
            error: Some(error),
        }
    }

    fn disarm(mut self) {
        self.error = None;
    }
}

impl Drop for Unwinding<'_> {
    fn drop(&mut self) {
        if let Some(error) = self.error.take() {
            let mut inner = self.core.lock();
            if matches!(inner.state, CaptureState::Stopping) {
                self.core.set_state(
                    &mut inner,
                    &CaptureState::Failed {
                        error,
                        recording: None,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The window between a finalise's wakeup and the waiting `stop()`'s
    //! next hold of the lock, forced through `before_stop_waits`: the
    //! public API in `tests/session.rs` can only race for it. Swift's actor
    //! ran the queued stop right after the finalise, so Swift has no
    //! counterpart.

    use std::path::Path;

    use super::*;
    use crate::capture::CaptureMode;
    use crate::testing::SyntheticCaptureBackend;
    use crate::writer::{LaneFrames, RecordingFiles};

    const RECV: Duration = Duration::from_secs(10);

    /// The production writer, except that every write fails as a full disk
    /// does and `finish()` waits for `release`, which holds the writer
    /// failure's finalise in `Stopping`.
    struct FailsThenHolds {
        inner: RecordingWriter,
        release: Receiver<()>,
    }

    impl RecordingWriting for FailsThenHolds {
        fn files(&self) -> RecordingFiles {
            self.inner.files()
        }
        fn write(&mut self, _frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            Err(CaptureError::WriterFailed("DiskFull".into()))
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.inner.sync()
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            // Bounded, so a failed test cannot hang its session's drop.
            let _ = self.release.recv_timeout(RECV);
            self.inner.finish()
        }
    }

    /// Receives from `states` until a state matches `want`.
    fn wait_for(states: &Receiver<CaptureState>, want: impl Fn(&CaptureState) -> bool) {
        loop {
            let state = states.recv_timeout(RECV).expect("the state changes");
            if want(&state) {
                return;
            }
        }
    }

    /// The first recording's writer fails and its finalise holds
    /// `Stopping`; a `stop()` finds it there and pauses in
    /// `before_stop_waits` while the finalise ends `Failed` and, given a
    /// `second` meeting, a `start()` for it takes the lock. Returns the
    /// session, what that `stop()` returned and the first meeting.
    fn stop_that_waited(
        directory: &Path,
        second: Option<Uuid>,
    ) -> (CaptureSession, Result<CaptureResult, CaptureError>, Uuid) {
        let (release, released) = channel();
        let first_writer = Mutex::new(Some(released));
        let session = CaptureSession::with_writer_factory(
            CaptureConfiguration::new(CaptureMode::InPerson, directory),
            Arc::new(SyntheticCaptureBackend::tones(
                &[AudioLane::Mixed],
                &[(AudioLane::Mixed, 440.0)],
                0.5,
            )),
            None,
            CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(SystemClock::new()),
            Arc::new(move |layout, lanes, keep_raw| {
                let inner = RecordingWriter::new(layout, lanes, keep_raw)?;
                Ok(match first_writer.lock().unwrap().take() {
                    Some(release) => Box::new(FailsThenHolds { inner, release }),
                    None => Box::new(inner) as Box<dyn RecordingWriting>,
                })
            }),
        )
        .unwrap();
        let states = session.states();
        let first = Uuid::new_v4();
        session.start(first).unwrap();
        wait_for(&states, |state| matches!(state, CaptureState::Stopping));

        let (paused, pause) = channel();
        let (resume, resumed) = channel::<()>();
        session.core.lock().before_stop_waits = Some(Box::new(move || {
            paused.send(()).unwrap();
            resumed.recv_timeout(RECV).unwrap();
        }));
        let stopped = std::thread::scope(|scope| {
            let stopper = scope.spawn(|| session.stop());
            pause
                .recv_timeout(RECV)
                .expect("the stop() finds the finalise in Stopping");
            release.send(()).unwrap();
            wait_for(&states, |state| {
                matches!(state, CaptureState::Failed { .. })
            });
            if let Some(second) = second {
                session.start(second).unwrap();
            }
            resume.send(()).unwrap();
            stopper.join().unwrap()
        });
        (session, stopped, first)
    }

    /// A `start()` takes the lock between the finalise's `Failed` and the
    /// waiting `stop()`: the stop answers `InvalidState` and leaves the new
    /// recording running, instead of ending a meeting it was never asked
    /// to end.
    #[test]
    fn a_stop_that_waited_does_not_stop_the_next_recording() {
        let directory = tempfile::tempdir().unwrap();
        let second = Uuid::new_v4();
        let (session, stopped, _) = stop_that_waited(directory.path(), Some(second));
        assert_eq!(
            stopped.unwrap_err(),
            CaptureError::InvalidState("stop after another recording started".into())
        );
        assert!(
            matches!(session.state(), CaptureState::Recording { .. }),
            "{:?}",
            session.state()
        );
        assert_eq!(session.stop().unwrap().asset.meeting_id, second);
        assert_eq!(session.state(), CaptureState::Idle);
    }

    /// The waiting `stop()` holds the lock again before any `start()`: it
    /// returns the recording `Failed` carries, and the next recording
    /// starts and stops on its own.
    #[test]
    fn a_stop_that_waited_returns_the_failed_recording() {
        let directory = tempfile::tempdir().unwrap();
        let (session, stopped, first) = stop_that_waited(directory.path(), None);
        let result = stopped.expect("stop() returns the finalised recording");
        assert_eq!(result.asset.meeting_id, first);
        match session.state() {
            CaptureState::Failed {
                error: CaptureError::WriterFailed(detail),
                recording,
            } => {
                assert!(detail.contains("DiskFull"), "{detail}");
                assert_eq!(recording.as_deref(), Some(&result));
            }
            other => panic!("expected Failed(WriterFailed), got {other:?}"),
        }
        let second = Uuid::new_v4();
        session.start(second).unwrap();
        assert_eq!(session.stop().unwrap().asset.meeting_id, second);
        assert_eq!(session.state(), CaptureState::Idle);
    }
}
