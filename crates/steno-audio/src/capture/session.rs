//! The recording state machine over a [`CaptureBackend`]: `Idle → Starting
//! → Recording → Stopping → Idle`, or `Failed` with the partial recording.
//! A device change or a stall rebuilds the backend while the recording goes
//! on, filling the gap with silence. One mutex guards the state; no thread
//! is joined while it is held, bar the PipeWire thread of a failed start.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
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
use super::level_publisher::LevelPublisher;
use super::start_log::{self, start_log};
use crate::aec::SpeexEchoCanceller;
use crate::clock::{Cancel, Clock, SystemClock};
use crate::playback::{Playback, RecordingHold};
use crate::realtime::{
    FrameRelay, LaneFrameSink, LevelSlot, ProcessingConfiguration, ProcessingThread, RateConverter,
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

/// A recording session over one backend. Tests run the same pipeline over
/// a synthetic backend and a manual clock ([`CaptureSession::with_backend`]).
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
    /// No in-app playback while recording: the hold is taken before the
    /// backend starts, since a Mac call capture's tap hears Steno itself.
    playback: Playback,
    inner: Mutex<Inner>,
    /// Notified on every state change; `stop()` waits on it while another
    /// thread's finalise holds `Stopping`.
    state_changed: Condvar,
    /// For tests ([`CaptureSession::refuse_the_writer_failure_thread`]):
    /// a failed write finds no thread to end the recording on.
    refuse_writer_failure_thread: AtomicBool,
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
    /// The newest recording a `Failed` state carried, with the start it
    /// belongs to: the answer of a `stop()` that waited out its finalise
    /// while a `start()` took the lock first.
    finalised: Option<(usize, Box<CaptureResult>)>,
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
    /// Hands the slot's levels to the subscribers from its own thread, so a
    /// slow sync never freezes the meter. Runs across rebuilds.
    level_publisher: LevelPublisher,
    ended_on_device_loss: bool,
    device_changes: usize,
    gap_seconds: f64,
    /// The loudest system-lane sample of the processing threads replaced.
    system_peak_so_far: f32,
    rebuild: Option<Rebuild>,
    /// A change reported while that rebuild ran; `resume` starts the next
    /// rebuild from it.
    pending_change: Option<DeviceChangeReason>,
    ring_counts: RingCounts,
    watch: Option<Watch>,
    /// The hold on [`Playback`], kept across rebuilds and dropped after
    /// `backend.stop()`.
    _recording_hold: Option<RecordingHold>,
    streak: Streak,
}

/// The restarts of a rebuild, and of the rebuilds after it while the
/// stream each resumed on stalls again soon: one count, one backoff and one
/// warning across them.
#[derive(Debug, Default)]
struct Streak {
    attempts: usize,
    /// `StillRestarting` went out and no `Delivering` since.
    warned: bool,
    /// When the latest `warn` line about the restarts went out, on the
    /// clock; kept when audio comes, so the log stays about once a minute.
    logged_at: Option<Duration>,
    /// A `warn` line went out and no line since says audio is back.
    unanswered: bool,
    gap_seconds: f64,
}

impl Streak {
    /// Audio has come after the streak's restarts: logs one `info` line if
    /// a `warn` line stood unanswered, and returns whether the warning
    /// stood.
    fn delivered(&mut self) -> bool {
        if std::mem::take(&mut self.unanswered) {
            tracing::info!(
                "the capture delivers again after {} restarts; {:.1} s of silence filled the gaps",
                self.attempts,
                self.gap_seconds
            );
        }
        std::mem::take(&mut self.warned)
    }
}

/// Per-lane counts the sink keeps at the device's rate, held in samples at
/// [`SAMPLE_RATE`]; each stream's share is rescaled at its own rate.
#[derive(Default)]
struct LaneCounts {
    seen: BTreeMap<AudioLane, usize>,
    at_output_rate: BTreeMap<AudioLane, usize>,
}

impl LaneCounts {
    /// Adds what `counts` gained since the last fold at `rate`; returns the
    /// increase per lane at [`SAMPLE_RATE`].
    fn fold(
        &mut self,
        counts: BTreeMap<AudioLane, usize>,
        rate: f64,
    ) -> BTreeMap<AudioLane, usize> {
        let mut added = BTreeMap::new();
        for (lane, count) in counts {
            let seen = self.seen.entry(lane).or_default();
            let samples = CaptureStream::rescaled(count.saturating_sub(*seen), rate, SAMPLE_RATE);
            *self.at_output_rate.entry(lane).or_default() += samples;
            *seen = count;
            added.insert(lane, samples);
        }
        added
    }

    fn most_seconds(&self) -> f64 {
        self.at_output_rate.values().max().copied().unwrap_or(0) as f64 / SAMPLE_RATE
    }
}

/// The sink's per-lane counts of one recording: ring overruns and missed
/// audio, filled with zeros or not.
#[derive(Default)]
struct RingCounts {
    dropped: LaneCounts,
    filled: LaneCounts,
    unfilled: LaneCounts,
    /// Counts before the first frames: unfilled, but never in a gap.
    before_first: LaneCounts,
    /// What a stalled stream's thread counted unfilled after the last frame
    /// the watch thread saw: time the rebuild's gap writes as silence too.
    unfilled_in_gap: BTreeMap<AudioLane, usize>,
}

impl RingCounts {
    fn fold(&mut self, sink: &LaneFrameSink, rate: f64) {
        self.dropped.fold(sink.dropped_samples(), rate);
        self.filled.fold(sink.filled_samples(), rate);
        self.unfilled.fold(sink.unfilled_samples(), rate);
        for (lane, samples) in self.before_first.fold(sink.before_first_samples(), rate) {
            *self.unfilled.at_output_rate.entry(lane).or_default() += samples;
        }
    }

    /// [`Self::fold`] for a rebuild's stop, keeping apart what the stalled
    /// thread counted unfilled after `before` for [`Self::gap_written`].
    fn fold_at_a_rebuild(
        &mut self,
        sink: &LaneFrameSink,
        before: Option<BTreeMap<AudioLane, usize>>,
        rate: f64,
    ) {
        self.unfilled_in_gap.clear();
        if let Some(before) = before {
            self.unfilled.fold(before, rate);
            self.unfilled_in_gap = self.unfilled.fold(sink.unfilled_samples(), rate);
        }
        self.fold(sink, rate);
    }

    /// The gap is written, `samples` per lane: that much of the stalled
    /// thread's late count is taken back, so the time counts once.
    fn gap_written(&mut self, samples: usize) {
        for (lane, late) in std::mem::take(&mut self.unfilled_in_gap) {
            if let Some(unfilled) = self.unfilled.at_output_rate.get_mut(&lane) {
                *unfilled -= late.min(samples).min(*unfilled);
            }
        }
    }

    /// A restart lost `samples` per lane before its first frame, which no
    /// gap wrote: counted unfilled on every one of `lanes`.
    fn restart_missed(&mut self, lanes: &[AudioLane], samples: usize) {
        if samples > 0 {
            for lane in lanes {
                *self.unfilled.at_output_rate.entry(*lane).or_default() += samples;
            }
        }
    }

    fn dropped_frames(&self) -> BTreeMap<AudioLane, usize> {
        self.dropped
            .at_output_rate
            .iter()
            .map(|(lane, samples)| (*lane, samples / FRAME_SIZE))
            .collect()
    }

    fn filled_seconds(&self) -> f64 {
        self.filled.most_seconds()
    }

    fn unfilled_seconds(&self) -> f64 {
        self.unfilled.most_seconds()
    }

    /// Logs the missed audio after the stop, if any.
    fn log_missed(&self) {
        let (filled, unfilled) = (self.filled_seconds(), self.unfilled_seconds());
        if filled > 0.0 || unfilled > 0.0 {
            tracing::warn!(
                filled = ?self.filled.at_output_rate,
                unfilled = ?self.unfilled.at_output_rate,
                "the capture's thread missed {:.3} s of audio: {filled:.3} s written as \
                 silence, {unfilled:.3} s too long to fill and left out",
                filled + unfilled
            );
        }
    }
}

/// The watch thread of one recording and its state under the lock.
struct Watch {
    cancel: Cancel,
    thread: JoinHandle<()>,
    progress: Progress,
    /// Whether the backend can probe a microphone on a stream of its own
    /// ([`CaptureBackend::probes_inputs`]).
    probes: bool,
    /// When to ask whether the chosen microphone delivers again, while
    /// recording on the default in its place.
    recheck: Option<Duration>,
    asked_after: Option<Duration>,
    resumed_at: Option<Duration>,
    /// When frames first came after that resume; a standing warning ends
    /// once they have come for `STALLED_AGAIN`.
    delivering_since: Option<Duration>,
}

impl Watch {
    /// Schedules the next ask: `CHOSEN_INPUT_RECHECK` the first time, then
    /// twice the latest wait, up to `CHOSEN_INPUT_RECHECK_LONGEST`.
    fn ask_again(&mut self, now: Duration) {
        let wait = self
            .asked_after
            .map_or(CaptureSession::CHOSEN_INPUT_RECHECK, |wait| {
                (wait * 2).min(CaptureSession::CHOSEN_INPUT_RECHECK_LONGEST)
            });
        self.asked_after = Some(wait);
        self.recheck = Some(now + wait);
    }
}

#[derive(Debug, Clone, Copy)]
struct Progress {
    /// The count last seen, and when it first had that value.
    count: usize,
    at: Duration,
}

struct Rebuild {
    cancel: Cancel,
    /// Returns the gap frames no `resume` counted, the stopped processing
    /// thread's system-lane peak and a restarted stream's rate no `resume`
    /// took.
    thread: JoinHandle<(usize, f32, Option<f64>)>,
}

/// A started stream, and whether it is the default input in place of the
/// chosen microphone.
struct Started {
    stream: CaptureStream,
    on_the_default: bool,
    /// For a restart whose frames came during its start: the time from the
    /// gap's end to the first frame, which nothing covers. Zero otherwise.
    late: Duration,
}

enum Restart {
    Started {
        started: Started,
        attempt: usize,
        /// Where the gap ends, on the clock.
        at: Duration,
    },
    Abandoned,
    Exhausted,
}

#[derive(Debug, Clone, Copy)]
struct RestartPlan {
    on_the_fallback: bool,
    /// Try the default first: the chosen microphone stalled again soon
    /// after the last rebuild resumed on it.
    default_first: bool,
    /// Only then does a restart that [`CaptureError::DidNotRun`] try the
    /// default input ([`CaptureBackend::stall_may_be_the_microphone`]).
    stall_may_be_the_microphone: bool,
    until_it_runs: bool,
    /// A start counts only once the sink saw a frame within `STALL_TIMEOUT`.
    awaits_delivery: bool,
    /// Restarts of the streak this rebuild continues, 0 for a new one.
    continues: usize,
}

impl CaptureSession {
    /// The waits before the second, third and fourth restart: a Bluetooth
    /// device is gone for one to two seconds while it changes profile.
    pub const RESTART_BACKOFF: [Duration; 3] = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
    ];
    /// Restarts tried before the recording ends in `DeviceLost`, unless
    /// the restarts go on until one runs (a stall, the audio service).
    pub const RESTART_ATTEMPTS: usize = Self::RESTART_BACKOFF.len() + 1;
    /// The longest wait between restarts past `RESTART_ATTEMPTS`: each wait
    /// is audio lost once the devices are back.
    pub const RESTART_BACKOFF_LONGEST: Duration = Duration::from_secs(4);

    /// The wait after the failed restart numbered `attempt` (1 for the
    /// first): `RESTART_BACKOFF`, then doubling up to
    /// `RESTART_BACKOFF_LONGEST`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use steno_audio::CaptureSession;
    ///
    /// assert_eq!(CaptureSession::restart_backoff(1), Duration::from_millis(250));
    /// assert_eq!(CaptureSession::restart_backoff(4), Duration::from_secs(2));
    /// assert_eq!(CaptureSession::restart_backoff(9), Duration::from_secs(4));
    /// ```
    #[must_use]
    pub fn restart_backoff(attempt: usize) -> Duration {
        let steps = Self::RESTART_BACKOFF.len();
        let step = attempt.max(1) - 1;
        if step < steps {
            return Self::RESTART_BACKOFF[step];
        }
        // The longest is reached long before the shift could overflow.
        let doublings = u32::try_from(step + 1 - steps).map_or(8, |doublings| doublings.min(8));
        (Self::RESTART_BACKOFF[steps - 1] * (1 << doublings)).min(Self::RESTART_BACKOFF_LONGEST)
    }
    /// The most silence written for one gap; a longer outage leaves the
    /// master short of wall time. The gap is written while the restarted
    /// stream's audio waits in the sink's 2 s rings, so it must drain fast.
    pub const MAXIMUM_GAP: Duration = Duration::from_secs(10);
    /// How long a watched backend may deliver nothing before the watch
    /// thread reports [`DeviceChangeReason::DeliveryStalled`]. Longer than
    /// the backends take to report a device change themselves; costs no
    /// audio, since the gap is filled from the last frame seen.
    pub const STALL_TIMEOUT: Duration = Duration::from_secs(2);
    /// How often the watch thread samples the sink: a stall's resolution.
    pub const STALL_CHECK_INTERVAL: Duration = Duration::from_millis(100);
    /// How long a recording runs on the default in place of a chosen
    /// microphone that did not open before the session asks whether the
    /// chosen one delivers ([`CaptureBackend::probes_inputs`] only).
    pub const CHOSEN_INPUT_RECHECK: Duration = Duration::from_secs(5);
    /// The longest wait between those asks; each ask doubles the wait.
    pub const CHOSEN_INPUT_RECHECK_LONGEST: Duration = Duration::from_secs(80);
    /// Frames the writer may fall behind processing before frames drop:
    /// 20 s, so a disk that stalls for seconds loses nothing.
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

    /// `echo_canceller` `None` in `Call` with echo cancellation on means
    /// [`SpeexEchoCanceller`]. `writer_headroom_frames` is the relay depth;
    /// `clock` paces the restart backoff and measures gaps.
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

    /// As [`Self::with_backend`], with the file writer injected too.
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
                playback: Playback::global(),
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
                    finalised: None,
                }),
                state_changed: Condvar::new(),
                refuse_writer_failure_thread: AtomicBool::new(false),
            }),
        })
    }

    /// The session with its own [`Playback`] gate instead of
    /// [`Playback::global`], for tests. Call it before the first `start`.
    ///
    /// # Panics
    ///
    /// When the session has started a recording (its core is shared then).
    #[doc(hidden)]
    #[must_use]
    pub fn with_playback(mut self, playback: Playback) -> Self {
        Arc::get_mut(&mut self.core)
            .expect("with_playback before the first start")
            .playback = playback;
        self
    }

    #[must_use]
    pub fn configuration(&self) -> &CaptureConfiguration {
        &self.core.configuration
    }

    #[must_use]
    pub fn writer_headroom_frames(&self) -> usize {
        self.core.writer_headroom_frames
    }

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

    /// Whether a write failed in the recording in progress. The session
    /// ends such a recording itself on its own thread; when that thread
    /// could not be spawned the state stays `Recording` and a caller that
    /// polls this stops it.
    #[must_use]
    pub fn write_failed(&self) -> bool {
        let inner = self.core.lock();
        matches!(inner.state, CaptureState::Recording { .. })
            && inner
                .active
                .as_ref()
                .and_then(|active| active.writer_thread.as_ref())
                .is_some_and(WriterThread::has_failed)
    }

    /// For tests: a failed write finds no thread to end the recording on.
    #[doc(hidden)]
    pub fn refuse_the_writer_failure_thread(&self) {
        self.core
            .refuse_writer_failure_thread
            .store(true, Ordering::SeqCst);
    }

    /// Starts a recording for `meeting_id` in its own folder of the configured
    /// directory; `InvalidState` while one is starting, recording or stopping.
    pub fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError> {
        self.core.start(meeting_id)
    }

    /// Ends the recording and returns it, abandoning a rebuild in flight.
    /// After `Failed`, returns the partial recording the state carries;
    /// while a finalise runs, waits for it and returns its recording. A
    /// failed write, close or sync leaves `Failed` with the recording
    /// returned. Fails with `WriterFailed` when the master is gone from disk
    /// or the writer thread died.
    pub fn stop(&self) -> Result<CaptureResult, CaptureError> {
        self.core.stop()
    }

    /// The sink's handler. Ignored unless recording; during a rebuild the
    /// reason is kept for `resume`; otherwise a rebuild starts on its own
    /// thread. Public so tests can report a change.
    pub fn device_changed(&self, reason: DeviceChangeReason) {
        self.core.device_changed(reason);
    }

    /// The far-end delay for the backend's latencies: both device paths in
    /// full, leaving the Speex tail for the room. Never rounded up: the MDF
    /// filter cannot recover from over-delay.
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
        let samples = (gap.as_secs_f64() * SAMPLE_RATE).floor() as usize;
        samples / FRAME_SIZE
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let recording = matches!(self.core.lock().state, CaptureState::Recording { .. });
        if recording {
            let _ = self.core.stop();
        }
    }
}

impl Core {
    /// A stream that stalls again this soon after a rebuild resumed on it
    /// continues that rebuild's streak, and a chosen microphone that does
    /// is replaced by the default next time. A standing warning ends once
    /// audio has come for this long.
    const STALLED_AGAIN: Duration = Duration::from_secs(10);
    /// How often, at most, failing restarts are logged above `debug`.
    const RESTART_LOG_INTERVAL: Duration = Duration::from_secs(60);

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_state(&self, inner: &mut Inner, state: &CaptureState) {
        if let CaptureState::Failed {
            recording: Some(recording),
            ..
        } = state
        {
            inner.finalised = Some((inner.recordings_started, recording.clone()));
        }
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
        // The latencies are device frames; the canceller runs at 48 kHz.
        CaptureSession::far_end_delay_frames(
            stream.at_output_rate(stream.input_latency_frames),
            stream.at_output_rate(stream.output_latency_frames),
        )
    }

    fn keep_raw(&self) -> bool {
        self.configuration.keep_raw_mic_lane && self.configuration.lanes().contains(&AudioLane::Mic)
    }

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
        // A rate the converter cannot take is recorded unconverted rather
        // than panicking with the lock held.
        configuration.device_rate = if RateConverter::supports(stream.sample_rate) {
            stream.sample_rate
        } else {
            tracing::error!(
                "the capture runs at {} Hz, which cannot be converted; recording it unconverted",
                stream.sample_rate
            );
            SAMPLE_RATE
        };
        configuration.keep_raw_mic = self.keep_raw();
        ProcessingThread::new(Arc::clone(sink), Arc::clone(relay), configuration, levels)
    }

    /// [`Self::start_recording`] under a guard: a panic leaves `Failed`
    /// with no recording and the backend stopped, not `Starting` for good.
    /// The [`Playback`] hold is declared before the guard, so it is
    /// released only after the guard's `backend.stop()`.
    fn start(self: &Arc<Self>, meeting_id: Uuid) -> Result<(), CaptureError> {
        let mut hold = Some(self.playback.hold_for_recording());
        let unwinding = Unwinding::starting(self);
        let started = self.start_recording(meeting_id, &mut hold);
        unwinding.disarm();
        started
    }

    /// Moves `hold` into the recording once it runs.
    #[allow(clippy::too_many_lines)]
    fn start_recording(
        self: &Arc<Self>,
        meeting_id: Uuid,
        hold: &mut Option<RecordingHold>,
    ) -> Result<(), CaptureError> {
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
        let started = self
            .backend
            .start(
                &lanes,
                self.configuration.input_device_uid.as_deref(),
                Arc::clone(&sink),
            )
            .map(|stream| Started {
                stream,
                on_the_default: false,
                late: Duration::ZERO,
            })
            .or_else(|error| {
                self.start_on_the_default(&sink)
                    .map(|stream| Started {
                        stream,
                        on_the_default: true,
                        late: Duration::ZERO,
                    })
                    .ok_or(error)
            });
        let Started {
            stream,
            on_the_default,
            ..
        } = match started {
            Ok(started) => started,
            Err(error) => {
                let mut writer = writer;
                let _ = writer.finish();
                // Only a folder this start made: never a meeting's files.
                if writer.created_directory() {
                    let _ = std::fs::remove_dir_all(&layout.directory);
                }
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
            lanes.len(),
            keep_raw,
            Box::new(move |error| {
                // Finalising joins the writer thread, so it runs on its own.
                if let Some(core) = on_error.upgrade() {
                    let spawned = if core.refuse_writer_failure_thread.load(Ordering::SeqCst) {
                        Err(std::io::Error::other("refused for a test"))
                    } else {
                        std::thread::Builder::new()
                            .name("steno-wfail".into())
                            .spawn(move || core.writer_failed(&error, recording))
                            .map(drop)
                    };
                    // No thread to finalise on: `write_failed` tells, and
                    // `stop()` returns the recording with the failure.
                    if let Err(spawn) = spawned {
                        tracing::error!(%spawn, "a write failed and the recording could not be ended");
                    }
                }
            }),
        );
        writer_thread.start();

        let watch = self.backend.delivers_continuously(&lanes).then(|| {
            let probes = self.backend.probes_inputs();
            let cancel = Cancel::new();
            let token = cancel.clone();
            let core = Arc::clone(self);
            let thread = std::thread::Builder::new()
                .name("steno-watch".into())
                .spawn(move || core.watch(recording, &token))
                .expect("spawn watch thread");
            let now = self.clock.now();
            let mut watch = Watch {
                cancel,
                thread,
                progress: Progress { count: 0, at: now },
                probes,
                recheck: None,
                asked_after: None,
                resumed_at: None,
                delivering_since: None,
            };
            if probes && Self::replaced_the_chosen(&stream, on_the_default) {
                watch.ask_again(now);
            }
            watch
        });
        // Last before the processing thread: until it runs nothing
        // publishes, so a panic there drops a publisher that joins at once.
        let level_publisher = LevelPublisher::start(
            Arc::clone(&levels),
            Box::new(move |levels| {
                if let Some(core) = on_levels.upgrade() {
                    core.publish(levels);
                }
            }),
        );
        processing.start();
        inner.active = Some(Active {
            meeting_id,
            stream,
            sink,
            relay,
            processing: Some(processing),
            writer_thread: Some(writer_thread),
            levels,
            level_publisher,
            ended_on_device_loss: false,
            device_changes: 0,
            gap_seconds: 0.0,
            system_peak_so_far: 0.0,
            rebuild: None,
            pending_change: None,
            ring_counts: RingCounts::default(),
            watch,
            _recording_hold: hold.take(),
            streak: Streak::default(),
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
            // Wait out another thread's finalise, re-checking after every
            // wakeup.
            let mut inner = self.lock();
            let recording = inner.recordings_started;
            while matches!(inner.state, CaptureState::Stopping) {
                inner = self
                    .state_changed
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            // A `start()` took the lock first: answer with the finalised
            // recording and leave the new one running.
            if inner.recordings_started != recording {
                return match &inner.finalised {
                    Some((finalised, result)) if *finalised == recording => Ok((**result).clone()),
                    _ => Err(CaptureError::InvalidState(
                        "stop after another recording started".into(),
                    )),
                };
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
        // Closing can fail on a full disk; the master is still readable,
        // so the result carries the failure.
        let state = match &finished {
            Ok(result) => match &result.failure {
                None => CaptureState::Idle,
                Some(error) => CaptureState::Failed {
                    error: error.clone(),
                    recording: Some(Box::new(result.clone())),
                },
            },
            Err(error) => CaptureState::Failed {
                error: error.clone(),
                recording: None,
            },
        };
        self.set_state(&mut self.lock(), &state);
        unwinding.disarm();
        finished
    }

    /// Stops the processing thread (keeping its canceller) and then the
    /// level publisher. Returns the loudest system-lane sample.
    fn stop_processing(&self, active: &mut Active) -> f32 {
        let mut system_peak = active.system_peak_so_far;
        if let Some(mut processing) = active.processing.take() {
            processing.stop();
            system_peak = system_peak.max(processing.system_peak());
            self.lock().echo_canceller = processing.take_echo_canceller();
        }
        active.level_publisher.stop();
        system_peak
    }

    /// Abandons the rebuild, stops the backend, drains and closes, and
    /// builds the asset even when a write or close failed (the failure is
    /// in [`CaptureResult::failure`]). `WriterFailed` when the writer
    /// thread died or the master is gone from disk. Threads are stopped with
    /// the lock released.
    fn finish(&self) -> Result<CaptureResult, CaptureError> {
        let mut active = self
            .lock()
            .active
            .take()
            .ok_or_else(|| CaptureError::InvalidState("nothing to finish".into()))?;
        // The rate the rings carry: the stream's, or a restarted one's that
        // the stop overtook before its `resume`.
        let mut ring_rate = active.stream.sample_rate;
        // The watch thread waits on nothing but the lock and the clock, and
        // never finishes a recording itself, so the join cannot deadlock.
        if let Some(watch) = active.watch.take() {
            watch.cancel.cancel();
            let _ = watch.thread.join();
        }
        if let Some(rebuild) = active.rebuild.take() {
            rebuild.cancel.cancel();
            // With `active` gone every step of the rebuild gives up. The
            // join must come before the processing thread's stop, the
            // writer's and `clear()`.
            let (unaccounted, peak, restarted_rate) =
                rebuild.thread.join().unwrap_or((0, 0.0, None));
            ring_rate = restarted_rate.unwrap_or(ring_rate);
            active.gap_seconds += Self::seconds(unaccounted);
            active.system_peak_so_far = active.system_peak_so_far.max(peak);
        }
        self.backend.stop();
        let system_peak = self.stop_processing(&mut active);
        let writer_lost =
            || CaptureError::WriterFailed("the writer thread ended without its files".into());
        let mut writer_thread = active.writer_thread.take().ok_or_else(writer_lost)?;
        writer_thread.stop();
        // A write that failed during this drain comes back beside the
        // asset, as a failed close does; a sync failure ranks behind both.
        let write_failure = writer_thread.take_error();
        let sync_failure = writer_thread.take_sync_error();
        let mut writer = writer_thread.take_writer().ok_or_else(writer_lost)?;
        // Read before `clear()` zeroes the counts. Whole frames still in the
        // rings never reached the relay and count as dropped.
        active.ring_counts.fold(&active.sink, ring_rate);
        let undrained =
            CaptureStream::rescaled(active.sink.available_to_read(), ring_rate, SAMPLE_RATE)
                / FRAME_SIZE;
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
        let mut dropped = active.ring_counts.dropped_frames();
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
            filled_seconds: active.ring_counts.filled_seconds(),
            unfilled_seconds: active.ring_counts.unfilled_seconds(),
        };
        active.ring_counts.log_missed();
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
        Ok(CaptureResult {
            asset,
            statistics,
            failure,
        })
    }

    // Device changes

    fn device_changed(self: &Arc<Self>, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        self.begin_rebuild(&mut inner, reason);
    }

    /// [`Self::device_changed`] for a report from the backend of
    /// `recording`; an older recording's report reaches nothing.
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
        let recording = inner.recordings_started;
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
        // `finish()` looks for it.
        let thread = match std::thread::Builder::new()
            .name("steno-rebuild".into())
            .spawn(move || core.rebuild(generation, recording, reason, &token))
        {
            Ok(thread) => thread,
            // Never a panic here: this runs on the backend's thread, which a
            // panic would end silently. The recording goes on and the latch
            // opens again, so the backend's next report retries.
            Err(spawn) => {
                tracing::error!(%spawn, ?reason, "a device change could not be followed");
                active.sink.rearm_device_change();
                return;
            }
        };
        active.rebuild = Some(Rebuild { cancel, thread });
        inner.rebuild_generation = generation;
        Self::emit(inner, CaptureNotice::DeviceChanged(reason));
    }

    /// Whether `stream` is the default input started in place of the
    /// chosen microphone.
    fn replaced_the_chosen(stream: &CaptureStream, on_the_default: bool) -> bool {
        on_the_default && stream.input.as_ref().is_some_and(|input| input.is_fallback)
    }

    /// The watch thread of the start numbered `recording`: every
    /// `STALL_CHECK_INTERVAL` it samples the sink's count of frames offered
    /// and reports a stream that offered nothing for `STALL_TIMEOUT`
    /// (`DeliveryStalled`), audio that came for `STALLED_AGAIN` after
    /// `StillRestarting` (`Delivering`), and, over a backend that probes, a
    /// chosen microphone that delivers again (`ChosenInputRecheck`). Judges
    /// nothing while a rebuild runs.
    fn watch(self: &Arc<Self>, recording: usize, cancel: &Cancel) {
        let mut probe: Option<JoinHandle<bool>> = None;
        while self
            .clock
            .sleep(CaptureSession::STALL_CHECK_INTERVAL, cancel)
        {
            let delivered = probe
                .take_if(|probe| probe.is_finished())
                .is_some_and(|probe| probe.join().unwrap_or(false));
            let (report, ask) = {
                let mut inner = self.lock();
                if inner.recordings_started != recording
                    || !matches!(inner.state, CaptureState::Recording { .. })
                {
                    return;
                }
                let now = self.clock.now();
                let Some(active) = inner.active.as_mut() else {
                    return;
                };
                let Some(watched) = active.watch.as_mut() else {
                    return;
                };
                if active.rebuild.is_some() {
                    continue;
                }
                let count = active.sink.frames_offered();
                let progress = &mut watched.progress;
                let mut reason = None;
                // Audio that came for `STALLED_AGAIN` ends a streak of
                // restarts.
                let mut delivering = false;
                if count != progress.count {
                    progress.count = count;
                    progress.at = now;
                    let since = *watched.delivering_since.get_or_insert(now);
                    if now.saturating_sub(since) >= Self::STALLED_AGAIN {
                        delivering = active.streak.delivered();
                    }
                } else if now.saturating_sub(progress.at) > CaptureSession::STALL_TIMEOUT {
                    reason = Some(DeviceChangeReason::DeliveryStalled);
                }
                let replaced = watched.recheck.is_some();
                if reason.is_none() && delivered && replaced {
                    reason = Some(DeviceChangeReason::ChosenInputRecheck);
                }
                let ask = reason.is_none()
                    && probe.is_none()
                    && watched.recheck.is_some_and(|due| now >= due);
                let ask = if ask {
                    watched.ask_again(now);
                    self.configuration.input_device_uid.clone()
                } else {
                    None
                };
                let report = reason.map(|reason| (Arc::clone(&active.sink), reason));
                if delivering {
                    Self::emit(&mut inner, CaptureNotice::Delivering);
                }
                (report, ask)
            };
            if let Some(uid) = ask {
                let backend = Arc::clone(&self.backend);
                probe = std::thread::Builder::new()
                    .name("steno-probe".into())
                    .spawn(move || backend.probe_input(&uid))
                    .inspect_err(|spawn| {
                        tracing::warn!(%spawn, "the chosen microphone could not be asked for");
                    })
                    .ok();
            }
            // Outside the lock: the sink's handler takes it.
            if let Some((sink, reason)) = report {
                sink.report_device_change(reason);
            }
        }
    }

    fn still_rebuilding(inner: &Inner, generation: usize) -> bool {
        matches!(inner.state, CaptureState::Recording { .. })
            && inner.active.as_ref().is_some_and(|a| a.rebuild.is_some())
            && inner.rebuild_generation == generation
    }

    /// [`Self::rebuild_steps`]; a panic in them ends the recording in
    /// `Failed(DeviceLost)`, saved, as a device that stayed lost does.
    fn rebuild(
        self: &Arc<Self>,
        generation: usize,
        recording: usize,
        reason: DeviceChangeReason,
        cancel: &Cancel,
    ) -> (usize, f32, Option<f64>) {
        // `AssertUnwindSafe` holds: every step changes `Inner` under the
        // lock in whole assignments, and `finish()` copes with what a panic
        // leaves half done outside it (backend, processing thread, rings).
        let steps = AssertUnwindSafe(|| self.rebuild_steps(generation, recording, reason, cancel));
        std::panic::catch_unwind(steps).unwrap_or_else(|_| {
            tracing::error!("following a device change panicked; the recording ends");
            self.device_lost(recording, generation);
            (0, 0.0, None)
        })
    }

    /// Stops the old backend and processing thread, restarts with backoff,
    /// writes the gap from the old stream's last frame as silence and
    /// starts a new processing thread; the sink, relay, writer and files
    /// stay. Returns what a stop that came first kept from `resume`: the
    /// gap frames, the old system-lane peak and the restarted rate.
    fn rebuild_steps(
        self: &Arc<Self>,
        generation: usize,
        recording: usize,
        reason: DeviceChangeReason,
        cancel: &Cancel,
    ) -> (usize, f32, Option<f64>) {
        // The gap runs from before the teardown, whose HAL calls are dead
        // time in the master too, or from the last frame the watch thread
        // saw when that came earlier.
        let now = self.clock.now();
        let mut silent_from = now;
        let mut unfilled_at_last_frame = None;
        let (sink, relay, processing, plan, quiet) = {
            let mut inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return (0, 0.0, None);
            }
            let Some(active) = inner.active.as_mut() else {
                return (0, 0.0, None);
            };
            if let Some(watch) = &active.watch
                && watch.progress.count == active.sink.frames_offered()
            {
                silent_from = silent_from.min(watch.progress.at);
                unfilled_at_last_frame = Some(active.sink.unfilled_samples());
            }
            let plan = self.restart_plan(active, reason, now);
            if plan.continues == 0 {
                active.streak = Streak {
                    warned: active.streak.warned,
                    unanswered: active.streak.unanswered,
                    ..Streak::default()
                };
                // While a `warn` line stands unanswered, the stall's own line
                // goes to `debug`.
                if reason == DeviceChangeReason::DeliveryStalled {
                    let millis = CaptureSession::STALL_TIMEOUT.as_millis();
                    if active.streak.unanswered {
                        tracing::debug!(
                            "the capture delivered nothing for over {millis} ms; restarting it"
                        );
                    } else {
                        tracing::warn!(
                            "the capture delivered nothing for over {millis} ms; restarting it"
                        );
                    }
                }
            }
            (
                Arc::clone(&active.sink),
                Arc::clone(&active.relay),
                active.processing.take(),
                plan,
                active.streak.logged_at.is_some(),
            )
        };
        // The processing thread's stop drains the rings' last audio into
        // the relay.
        start_log::quietly(quiet, || self.backend.stop());
        let mut canceller = None;
        let mut peak = 0.0f32;
        if let Some(mut processing) = processing {
            processing.stop();
            peak = processing.system_peak();
            canceller = processing.take_echo_canceller();
        }
        // New devices mean a new echo path: the filter starts cold.
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
                let rate = active.stream.sample_rate;
                active
                    .ring_counts
                    .fold_at_a_rebuild(&sink, unfilled_at_last_frame, rate);
                // Nothing writes the slot now; publish silence so the meter
                // does not hold the old level.
                active
                    .levels
                    .publish(LaneLevel::SILENCE, Some(LaneLevel::SILENCE));
            }
        }
        // The old backend lets no new report through, so the latch can
        // open: a report from the rebuilt backend is kept for `resume`.
        sink.rearm_device_change();
        let restart = self.restart_backend(&sink, plan, generation, cancel);
        let (unaccounted, restarted_rate) = match restart {
            Restart::Started {
                started,
                attempt,
                at,
            } => {
                let rate = started.stream.sample_rate;
                // The gap grows through every failed attempt and is written
                // once when a start succeeds.
                let elapsed = at.saturating_sub(silent_from);
                let gap_frames =
                    CaptureSession::gap_frames(elapsed.min(CaptureSession::MAXIMUM_GAP));
                let written = self.write_silence(gap_frames, &relay, generation, cancel);
                if written == gap_frames
                    && self.relay_has_room(&sink, &relay, &started.stream, generation, cancel)
                    && self.resume(started, attempt, gap_frames, &sink, &relay, generation)
                {
                    (0, None)
                } else {
                    (written, Some(rate))
                }
            }
            Restart::Abandoned => (0, None),
            Restart::Exhausted => {
                self.device_lost(recording, generation);
                (0, None)
            }
        };
        (unaccounted, peak, restarted_rate)
    }

    fn restart_plan(
        &self,
        active: &Active,
        reason: DeviceChangeReason,
        now: Duration,
    ) -> RestartPlan {
        let on_the_fallback = active
            .stream
            .input
            .as_ref()
            .is_some_and(|input| input.is_fallback);
        let watch = active.watch.as_ref();
        let stalled = reason == DeviceChangeReason::DeliveryStalled;
        let (soon, before_delivering) = watch
            .and_then(|watch| Some((watch.resumed_at?, watch.progress.at)))
            .map_or((false, false), |(resumed_at, delivered_at)| {
                (
                    now.saturating_sub(resumed_at) < Self::STALLED_AGAIN,
                    delivered_at <= resumed_at,
                )
            });
        // A chosen microphone that stalls again so soon would stall over
        // and over, unless the stall cannot be the microphone's.
        let stall_may_be_the_microphone = self
            .backend
            .stall_may_be_the_microphone(&self.configuration.lanes());
        let stalled_again = stalled
            && soon
            && stall_may_be_the_microphone
            && !on_the_fallback
            && self.configuration.input_device_uid.is_some();
        RestartPlan {
            on_the_fallback,
            default_first: stalled_again,
            stall_may_be_the_microphone,
            until_it_runs: matches!(
                reason,
                DeviceChangeReason::AudioServiceRestarted | DeviceChangeReason::DeliveryStalled
            ),
            awaits_delivery: watch.is_some(),
            continues: if stalled && (soon || before_delivering) {
                active.streak.attempts
            } else {
                0
            },
        }
    }

    /// Restarts with backoff (`CaptureSession::restart_backoff`), trying the
    /// default input after the last failure, or once early when waiting
    /// buys nothing (the stream was on the fallback, or the chosen
    /// microphone did not run and the stall may be its own). With
    /// `plan.default_first` the default goes first, once. A last restart
    /// that did not run, or any after a stall or an audio service restart
    /// (`plan.until_it_runs`), is not the end: restarts go on until one runs
    /// or the stop. `Exhausted` when they all failed, `Abandoned` when
    /// `stop()` cancelled or the recording is gone.
    fn restart_backend(
        &self,
        sink: &Arc<LaneFrameSink>,
        plan: RestartPlan,
        generation: usize,
        cancel: &Cancel,
    ) -> Restart {
        let mut default_tried = false;
        let mut attempt = plan.continues;
        let mut failure = "its stream stopped delivering after it resumed".to_owned();
        loop {
            if attempt > 0 {
                self.restart_failed(attempt, &failure, generation);
                if !self
                    .clock
                    .sleep(CaptureSession::restart_backoff(attempt), cancel)
                {
                    return Restart::Abandoned;
                }
            }
            attempt += 1;
            let last = attempt >= CaptureSession::RESTART_ATTEMPTS;
            if plan.default_first && !default_tried {
                default_tried = true;
                if let Ok(restart) = self.try_start(sink, true, attempt, plan, generation, cancel) {
                    return restart;
                }
            }
            let error = match self.try_start(sink, false, attempt, plan, generation, cancel) {
                Ok(restart) => return restart,
                Err(error) => error,
            };
            let did_not_run = matches!(error, CaptureError::DidNotRun(_));
            let early = !default_tried
                && (plan.on_the_fallback || (did_not_run && plan.stall_may_be_the_microphone));
            let default_may_help = !did_not_run || plan.stall_may_be_the_microphone;
            if (early || (last && default_may_help))
                && self.configuration.input_device_uid.is_some()
            {
                default_tried = true;
                if let Ok(restart) = self.try_start(sink, true, attempt, plan, generation, cancel) {
                    return restart;
                }
            }
            if last && !plan.until_it_runs && !did_not_run {
                return Restart::Exhausted;
            }
            failure = error.to_string();
        }
    }

    /// The restart numbered `attempt` failed with `failure`, and more
    /// follow. Logs above `debug` on the streak's first failure and about
    /// every `RESTART_LOG_INTERVAL` after; sends `StillRestarting` once the
    /// streak passed `RESTART_ATTEMPTS`, unless its warning already stands.
    fn restart_failed(&self, attempt: usize, failure: &str, generation: usize) {
        let mut inner = self.lock();
        if !Self::still_rebuilding(&inner, generation) {
            return;
        }
        let now = self.clock.now();
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        let streak = &mut active.streak;
        match streak.logged_at {
            None => {
                tracing::warn!("restart {attempt} of the capture failed ({failure}); trying again");
                streak.logged_at = Some(now);
                streak.unanswered = true;
            }
            Some(at) if now.saturating_sub(at) >= Self::RESTART_LOG_INTERVAL => {
                tracing::warn!(
                    "the capture is still restarting: {attempt} restarts so far, the latest \
                     failed ({failure})"
                );
                streak.logged_at = Some(now);
                streak.unanswered = true;
            }
            Some(_) => tracing::debug!("restart {attempt} of the capture failed ({failure})"),
        }
        if attempt >= CaptureSession::RESTART_ATTEMPTS && !streak.warned {
            streak.warned = true;
            Self::emit(&mut inner, CaptureNotice::StillRestarting { attempt });
        }
    }

    /// One start of a rebuild, on the chosen microphone or with
    /// `on_the_default` on the default. The start holds the mutex; with
    /// `plan.awaits_delivery` the wait for its first frame does not, and a
    /// stream that offered none within `STALL_TIMEOUT` is stopped and fails
    /// with `DidNotRun`. A stop during the wait answers `Started`, leaving
    /// the running stream to `finish()`.
    fn try_start(
        &self,
        sink: &Arc<LaneFrameSink>,
        on_the_default: bool,
        attempt: usize,
        plan: RestartPlan,
        generation: usize,
        cancel: &Cancel,
    ) -> Result<Restart, CaptureError> {
        let (stream, mut at, late, returned, offered, quiet) = {
            let inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return Ok(Restart::Abandoned);
            }
            let quiet = inner
                .active
                .as_ref()
                .is_some_and(|active| active.streak.logged_at.is_some());
            let offered = sink.frames_offered();
            let before = self.clock.now();
            let stream = start_log::quietly(quiet, || {
                if on_the_default {
                    self.start_on_the_default(sink)
                        .ok_or(CaptureError::InputDeviceUnavailable)
                } else {
                    self.backend.start(
                        &self.configuration.lanes(),
                        self.configuration.input_device_uid.as_deref(),
                        Arc::clone(sink),
                    )
                }
            })?;
            // Frames that came while `start` ran end the gap at the reading
            // before it; the time to the first frame is audio lost, counted
            // missed and not filled (`resume`).
            let returned = self.clock.now();
            let delivered = sink.frames_offered().wrapping_sub(offered);
            let (at, late) = if delivered == 0 {
                (returned, Duration::ZERO)
            } else {
                let covered = Duration::try_from_secs_f64(delivered as f64 / stream.sample_rate)
                    .unwrap_or(Duration::MAX);
                (
                    before,
                    returned.saturating_sub(before).saturating_sub(covered),
                )
            };
            (stream, at, late, returned, offered, quiet)
        };
        while plan.awaits_delivery && sink.frames_offered() == offered {
            let now = self.clock.now();
            if now.saturating_sub(returned) >= CaptureSession::STALL_TIMEOUT {
                start_log::quietly(quiet, || self.backend.stop());
                if let Some(active) = self.lock().active.as_mut() {
                    active.pending_change = None;
                }
                sink.rearm_device_change();
                return Err(CaptureError::DidNotRun(format!(
                    "the restarted capture delivered nothing within {} ms",
                    CaptureSession::STALL_TIMEOUT.as_millis()
                )));
            }
            at = now;
            if !self
                .clock
                .sleep(CaptureSession::STALL_CHECK_INTERVAL, cancel)
            {
                break;
            }
        }
        Ok(Restart::Started {
            started: Started {
                stream,
                on_the_default,
                late,
            },
            attempt,
            at,
        })
    }

    /// The default input in place of a chosen microphone whose `start`
    /// failed although it may be connected, marked as the fallback. `None`
    /// without a chosen microphone or when this start fails too. A backend
    /// started without a UID watches for no chosen device, so this cannot
    /// loop. Called with the mutex held.
    fn start_on_the_default(&self, sink: &Arc<LaneFrameSink>) -> Option<CaptureStream> {
        let chosen = self.configuration.input_device_uid.as_deref()?;
        let mut stream = self
            .backend
            .start(&self.configuration.lanes(), None, Arc::clone(sink))
            .inspect_err(|error| {
                start_log!(warn, "the default input did not start either: {error}");
            })
            .ok()?;
        if let Some(input) = stream.input.as_mut()
            && input.uid != chosen
        {
            start_log!(
                warn,
                "the input device {chosen} did not start; recording from the default input {}",
                input.uid
            );
            input.is_fallback = true;
        }
        Some(stream)
    }

    /// Starts the new processing thread on the kept sink and relay, records
    /// the statistics and sends the notice; a change reported during the
    /// rebuild starts the next one. `false` when a stop came first.
    fn resume(
        self: &Arc<Self>,
        Started {
            stream,
            on_the_default,
            late,
        }: Started,
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
        if let Some(watch) = active.watch.as_mut() {
            let now = self.clock.now();
            watch.progress.count = sink.frames_offered();
            watch.progress.at = now;
            watch.resumed_at = Some(now);
            watch.delivering_since = None;
            watch.recheck = None;
            if watch.probes && Self::replaced_the_chosen(&stream, on_the_default) {
                watch.ask_again(now);
            }
        }
        active.stream = stream;
        active.processing = Some(processing);
        active.device_changes += 1;
        active.gap_seconds += gap_seconds;
        active.ring_counts.gap_written(gap_frames * FRAME_SIZE);
        active
            .ring_counts
            .restart_missed(sink.lanes(), CaptureSession::gap_frames(late) * FRAME_SIZE);
        active.rebuild = None;
        active.streak.attempts = attempt;
        active.streak.gap_seconds += gap_seconds;
        // Unwatched, the resume stands for the first frame; watched, the
        // watch thread ends the warning after `STALLED_AGAIN`.
        let delivering = active.watch.is_none() && active.streak.delivered();
        Self::emit(
            &mut inner,
            CaptureNotice::DeviceResumed {
                attempt,
                gap_seconds,
            },
        );
        if delivering {
            Self::emit(&mut inner, CaptureNotice::Delivering);
        }
        // Must stay under this guard, or a stop and a new start in between
        // would hand this recording's change to the next. No test covers it.
        if let Some(pending) = pending {
            self.begin_rebuild(&mut inner, pending);
        }
        true
    }

    /// Zeros in every written channel for `frames` relay frames, through
    /// the relay; the sink's rings are too small for a long gap. A full
    /// relay is waited out in 5 ms steps; `has_room` is asked first because
    /// a refused `begin_frame` counts as dropped. Returns the frames
    /// written, fewer when the rebuild was abandoned.
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

    /// Waits until the relay has room for the frames the rings collected
    /// during the gap, so the new processing thread's push is not refused.
    /// `false` when the rebuild was abandoned.
    fn relay_has_room(
        &self,
        sink: &LaneFrameSink,
        relay: &FrameRelay,
        stream: &CaptureStream,
        generation: usize,
        cancel: &Cancel,
    ) -> bool {
        // The rings hold the restarted device's rate.
        let backlog = || {
            (stream.at_output_rate(sink.available_to_read()) / FRAME_SIZE)
                .min(relay.capacity_frames())
        };
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

    /// Every restart failed, or the rebuild panicked: the recording ends in
    /// `Failed(DeviceLost)`, but only when this rebuild of this start is
    /// still the newest.
    fn device_lost(&self, recording: usize, generation: usize) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. })
                || inner.recordings_started != recording
                || inner.rebuild_generation != generation
            {
                return;
            }
            let Some(active) = inner.active.as_mut() else {
                return;
            };
            active.ended_on_device_loss = true;
            // This runs inside the rebuild thread; `finish()` must not join it.
            active.rebuild = None;
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let unwinding = Unwinding::arm(self, CaptureError::DeviceLost);
        let state = match self.finish() {
            Ok(mut result) => CaptureState::Failed {
                error: result
                    .failure
                    .get_or_insert(CaptureError::DeviceLost)
                    .clone(),
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

    /// The writer thread of the start numbered `recording` failed; a late
    /// failure is ignored.
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
        let result = self.finish().ok().map(|mut result| {
            result.failure = Some(failure.clone());
            Box::new(result)
        });
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

/// `error` as a `WriterFailed`; a `WriterFailed` or `RecordingExists`
/// passes through.
fn as_writer_failure(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::WriterFailed(_) | CaptureError::RecordingExists(_) => error,
        other => CaptureError::WriterFailed(other.to_string()),
    }
}

/// Armed while a finalise holds `Stopping` or a start `Starting`. On a
/// panic its drop sets `Failed` with no recording and wakes waiting
/// `stop()`s; a start's also stops the backend.
struct Unwinding<'a> {
    core: &'a Core,
    error: Option<CaptureError>,
    starting: bool,
}

impl<'a> Unwinding<'a> {
    fn arm(core: &'a Core, error: CaptureError) -> Self {
        Self {
            core,
            error: Some(error),
            starting: false,
        }
    }

    fn starting(core: &'a Core) -> Self {
        Self {
            core,
            error: Some(CaptureError::BackendFailed(
                "starting the recording panicked".into(),
            )),
            starting: true,
        }
    }

    fn disarm(mut self) {
        self.error = None;
    }
}

impl Drop for Unwinding<'_> {
    fn drop(&mut self) {
        if let Some(error) = self.error.take() {
            // The backend stops with the lock released, as everywhere.
            let holds = if self.starting {
                CaptureState::Starting
            } else {
                CaptureState::Stopping
            };
            if self.core.lock().state != holds {
                return;
            }
            if self.starting {
                self.core.backend.stop();
            }
            self.core.set_state(
                &mut self.core.lock(),
                &CaptureState::Failed {
                    error,
                    recording: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Missed audio counts on the lane that missed most, each stream's
    /// share at its own rate.
    #[test]
    fn missed_audio_counts_on_the_lane_that_missed_most_at_each_streams_rate() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
        sink.ring(0).record_filled(4_800);
        sink.ring(1).record_filled(2_400);
        sink.ring(1).record_unfilled(48_000);
        sink.count_before_first(24_000, None);
        let mut counts = RingCounts::default();
        counts.fold(&sink, 24_000.0);
        sink.ring(1).record_filled(7_200);
        counts.fold(&sink, SAMPLE_RATE);
        assert_eq!(
            counts.filled_seconds(),
            0.25,
            "the system lane: 2 400 at 24 kHz and 7 200 at 48 kHz, above the microphone's 0.2 s"
        );
        assert_eq!(
            counts.unfilled_seconds(),
            3.0,
            "the system lane: 48 000 at 24 kHz, and 24 000 at 24 kHz before its first frame"
        );
    }

    /// A rebuild's gap takes back what the stalled thread counted unfilled,
    /// at most the gap's length per lane.
    #[test]
    fn a_rebuilds_gap_takes_back_the_late_count_on_every_lane() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
        let mut counts = RingCounts::default();
        let before = sink.unfilled_samples();
        sink.ring(0).record_unfilled(96_000);
        sink.ring(1).record_unfilled(120_000);
        counts.fold_at_a_rebuild(&sink, Some(before), SAMPLE_RATE);
        counts.gap_written(100_800);
        assert_eq!(
            counts.unfilled.at_output_rate,
            [(AudioLane::Mic, 0), (AudioLane::System, 19_200)].into()
        );
    }

    /// What a stream missed before its first frame is no part of the gap's
    /// late count.
    #[test]
    fn a_rebuilds_gap_never_takes_back_the_loss_before_the_first_frame() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic]);
        let mut counts = RingCounts::default();
        let before = sink.unfilled_samples();
        sink.ring(0).record_unfilled(48_000);
        sink.count_before_first(72_000, None);
        counts.fold_at_a_rebuild(&sink, Some(before), SAMPLE_RATE);
        counts.gap_written(100_800);
        assert_eq!(counts.unfilled_seconds(), 1.5);
    }

    /// What a rebuild kept apart for a gap no gap took back is gone at the
    /// next rebuild's fold.
    #[test]
    fn a_late_count_no_gap_took_back_stays_counted_at_the_next_rebuild() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic]);
        let mut counts = RingCounts::default();
        let before = sink.unfilled_samples();
        sink.ring(0).record_unfilled(48_000);
        counts.fold_at_a_rebuild(&sink, Some(before), SAMPLE_RATE);
        counts.fold_at_a_rebuild(&sink, None, SAMPLE_RATE);
        counts.gap_written(48_000);
        assert_eq!(counts.unfilled_seconds(), 1.0);
    }
}
