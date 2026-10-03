//! The state machine over a [`CaptureBackend`].
//! Swift: `Sources/StenoAudio/Capture/CaptureSession.swift`.
//!
//! `Idle → Starting → Recording → Stopping → Idle`, or `Failed` when a
//! device stays lost or the writer fails. Owns the sink, the processing
//! thread, the relay, the writer thread and the [`RecordingWriting`]
//! implementation; `stop()` tears them down in order (backend, processing,
//! writer, files) and returns the [`CaptureResult`]: the finished
//! [`AudioAsset`] (`Caf48kFloat32`, `sidecars_16k` filled, retention
//! `KeepForever` until the caller sets it from `Settings`) with statistics.
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
//! `Failed(DeviceLost)`.
//!
//! A recording cut short (device loss, writer failure, a full disk while
//! closing) is finalised and travels in the state: `Failed { error,
//! recording }`; `stop()` returns the same result, or fails when the
//! failure came from a start that produced nothing.
//!
//! # Threads
//!
//! Swift's actor becomes one mutex over the session state. The rule that
//! keeps it deadlock-free: **no thread is joined while the mutex is held**.
//! The backend's producer, the processing thread and the writer thread all
//! call back into the session (device changes, levels, write errors) and
//! take the mutex to do so; teardown therefore moves the recording out of
//! the state under the lock, releases it, and only then stops the threads.
//! The rebuild runs on its own thread, takes the mutex for each step that
//! touches the state and sleeps outside it on the injected [`Clock`], with
//! a [`Cancel`] token `stop()` raises.
//!
//! The one long hold is deliberate: `start` and the rebuild's
//! `restart_backend` keep the mutex across `backend.start()`, up to 200 ms
//! while [`NominalSampleRate::settle`](super::NominalSampleRate::settle)
//! waits for the aggregate. A `stop()` arriving meanwhile queues behind it
//! and then finds a started backend to tear down, instead of racing a
//! half-built one; a backend never calls back into the session from
//! `start`, so the hold cannot deadlock.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
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
use crate::{FRAME_SIZE, SAMPLE_RATE};

/// Opens the files for one recording; [`RecordingWriter::new`] in
/// production, a failure-injecting wrapper in tests.
pub type RecordingWriterFactory = Arc<
    dyn Fn(&RecordingLayout, &[AudioLane], bool) -> Result<Box<dyn RecordingWriting>, CaptureError>
        + Send
        + Sync,
>;

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
    /// The rebuild in flight, so `stop()` can abandon it.
    rebuild: Option<Cancel>,
    /// A change reported while that rebuild ran (the rebuilt backend's
    /// listeners are live before the gap is written); `resume` starts the
    /// next rebuild from it instead of losing it.
    pending_change: Option<DeviceChangeReason>,
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
    /// frames are dropped and counted: 200 (2 s) by default.
    pub const DEFAULT_WRITER_HEADROOM_FRAMES: usize = 200;

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
                }),
            }),
        })
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

    pub fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError> {
        self.core.start(meeting_id)
    }

    /// Ends the recording and returns it. A rebuild in flight is abandoned.
    /// After `Failed` returns the finalised partial recording the state
    /// carries, or fails when the failure came from a start that produced
    /// nothing.
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
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_state(inner: &mut Inner, state: &CaptureState) {
        inner.state = state.clone();
        inner
            .state_subscribers
            .retain(|s| s.send(state.clone()).is_ok());
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
        Self::set_state(&mut inner, &CaptureState::Starting);
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
                let failure = CaptureError::WriterFailed(error.to_string());
                Self::set_state(
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
                    core.device_changed(reason);
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
                Self::set_state(
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
                        .spawn(move || core.writer_failed(&error))
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
        Self::set_state(
            &mut inner,
            &CaptureState::Recording {
                started_at: Utc::now(),
            },
        );
        Ok(())
    }

    fn stop(&self) -> Result<CaptureResult, CaptureError> {
        {
            let mut inner = self.lock();
            match &inner.state {
                CaptureState::Recording { .. } => {}
                CaptureState::Failed { recording, .. } => {
                    return match recording {
                        Some(recording) => Ok((**recording).clone()),
                        None => Err(CaptureError::InvalidState(
                            "stop after a failed start".into(),
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
            Self::set_state(&mut inner, &CaptureState::Stopping);
        }
        let Some((result, failure)) = self.finish() else {
            return Err(CaptureError::InvalidState("nothing to finish".into()));
        };
        // Closing the files can fail on a full disk; the master is still
        // readable to its last frame, so the result comes back and the
        // state carries the failure instead of `stop()` throwing it away.
        let mut inner = self.lock();
        let state = match failure {
            Some(error) => CaptureState::Failed {
                error,
                recording: Some(Box::new(result.clone())),
            },
            None => CaptureState::Idle,
        };
        Self::set_state(&mut inner, &state);
        Ok(result)
    }

    /// Rebuild abandoned, backend off, rings drained, relay drained, files
    /// closed, asset built. The asset is built even when closing the files
    /// fails (its paths are fixed at start and the duration is what the
    /// master holds); the failure comes back beside it. The caller has set
    /// the state to `Stopping`; the threads are stopped with the lock
    /// released (see the module doc).
    fn finish(&self) -> Option<(CaptureResult, Option<CaptureError>)> {
        let mut active = self.lock().active.take()?;
        if let Some(rebuild) = active.rebuild.take() {
            rebuild.cancel();
        }
        self.backend.stop();
        let mut system_peak = active.system_peak_so_far;
        if let Some(mut processing) = active.processing.take() {
            processing.stop();
            system_peak = system_peak.max(processing.system_peak());
            self.lock().echo_canceller = processing.take_echo_canceller();
        }
        let mut writer_thread = active.writer_thread.take()?;
        writer_thread.stop();
        let mut writer = writer_thread.take_writer()?;
        active.sink.clear();
        let failure = match writer.finish() {
            Ok(_) => None,
            Err(error) => Some(CaptureError::WriterFailed(error.to_string())),
        };
        let files = writer.files();
        let lanes = self.configuration.lanes();
        let mut dropped: BTreeMap<AudioLane, usize> = BTreeMap::new();
        for (lane, samples) in active.sink.dropped_samples() {
            *dropped.entry(lane).or_default() += samples / FRAME_SIZE;
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
        Some((CaptureResult { asset, statistics }, failure))
    }

    // Device changes

    fn device_changed(self: &Arc<Self>, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        if !matches!(inner.state, CaptureState::Recording { .. }) {
            return;
        }
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        if active.rebuild.is_some() {
            active.pending_change = Some(reason);
            return;
        }
        let cancel = Cancel::new();
        active.rebuild = Some(cancel.clone());
        Self::emit(&mut inner, CaptureNotice::DeviceChanged(reason));
        inner.rebuild_generation += 1;
        let generation = inner.rebuild_generation;
        drop(inner);
        let core = Arc::clone(self);
        std::thread::Builder::new()
            .name("steno-rebuild".into())
            .spawn(move || core.rebuild(generation, &cancel))
            .expect("spawn rebuild thread");
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
    /// runs on a real-time thread.
    fn rebuild(self: &Arc<Self>, generation: usize, cancel: &Cancel) {
        // The stopwatch runs from before the teardown: the HAL calls in
        // `stop()` take tens to hundreds of milliseconds during a device
        // transition, and that is dead time in the master too.
        let started = self.clock.now();
        let (sink, relay, processing) = {
            let mut inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return;
            }
            let Some(active) = inner.active.as_mut() else {
                return;
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
        if let Some(mut processing) = processing {
            processing.stop();
            let peak = processing.system_peak();
            canceller = processing.take_echo_canceller();
            let mut inner = self.lock();
            if let Some(active) = inner.active.as_mut() {
                active.system_peak_so_far = active.system_peak_so_far.max(peak);
            }
        }
        // New devices mean a new echo path: the filter starts cold, as at start.
        if let Some(canceller) = canceller.as_mut() {
            canceller.reset();
        }
        {
            let mut inner = self.lock();
            inner.echo_canceller = canceller;
        }
        // The old backend's listeners went with it, so the latch can open
        // now: a report from the rebuilt backend before the gap is written
        // reaches `device_changed`, which keeps it for `resume`.
        sink.rearm_device_change();
        match self.restart_backend(&sink, generation, cancel) {
            Restart::Started(stream, attempt) => {
                // The gap grows through every failed attempt and is written
                // once, in full, when a start succeeds.
                let elapsed = self.clock.now().saturating_sub(started);
                let gap_frames =
                    CaptureSession::gap_frames(elapsed.min(CaptureSession::MAXIMUM_GAP));
                if !self.write_silence(gap_frames, &relay, generation, cancel)
                    || !self.relay_has_room(&sink, &relay, generation, cancel)
                {
                    return;
                }
                self.resume(stream, attempt, gap_frames, &sink, &relay, generation);
            }
            Restart::Abandoned => {}
            Restart::Exhausted => self.device_lost(),
        }
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
    /// rebuild starts the next one.
    fn resume(
        self: &Arc<Self>,
        stream: CaptureStream,
        attempt: usize,
        gap_frames: usize,
        sink: &Arc<LaneFrameSink>,
        relay: &Arc<FrameRelay>,
        generation: usize,
    ) {
        let pending = {
            let mut inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return;
            }
            let canceller = inner.echo_canceller.take();
            let Some(active) = inner.active.as_mut() else {
                return;
            };
            // Exact for any gap length.
            let gap_seconds = (gap_frames * FRAME_SIZE) as f64 / SAMPLE_RATE;
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
            pending
        };
        if let Some(pending) = pending {
            self.device_changed(pending);
        }
    }

    /// Zeros in every written channel for `frames` relay frames, through
    /// the relay the writer thread keeps draining. The rings under the sink
    /// are not touched: they hold two seconds and nothing drains them while
    /// the processing thread is stopped, so a longer gap would silently
    /// shrink into `dropped_samples`. A full relay (a long gap, or a writer
    /// still behind the old producer) is waited out in 5 ms steps on the
    /// clock; `has_room` is asked first because a refused `begin_frame`
    /// counts as a dropped frame. Returns `false` when the rebuild was
    /// abandoned meanwhile.
    fn write_silence(
        &self,
        frames: usize,
        relay: &FrameRelay,
        generation: usize,
        cancel: &Cancel,
    ) -> bool {
        if frames == 0 {
            return true;
        }
        let zeros = vec![0.0f32; relay.frame_size()];
        let mut remaining = frames;
        while remaining > 0 {
            if !Self::still_rebuilding(&self.lock(), generation) {
                return false;
            }
            if relay.has_room() && relay.begin_frame() {
                for channel in 0..relay.channels() {
                    relay.write(channel, &zeros);
                }
                relay.end_frame();
                remaining -= 1;
            } else if !self.clock.sleep(Duration::from_millis(5), cancel) {
                return false;
            }
        }
        true
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

    /// Every restart failed: the recording ends as it did before rebuilds
    /// existed, finalised and carried in `Failed(DeviceLost)`.
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
            // cancel it, and it is over anyway.
            active.rebuild = None;
            Self::set_state(&mut inner, &CaptureState::Stopping);
        }
        let Some((result, failure)) = self.finish() else {
            return;
        };
        let mut inner = self.lock();
        Self::set_state(
            &mut inner,
            &CaptureState::Failed {
                error: failure.unwrap_or(CaptureError::DeviceLost),
                recording: Some(Box::new(result)),
            },
        );
    }

    fn writer_failed(&self, error: &CaptureError) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. }) {
                return;
            }
            Self::set_state(&mut inner, &CaptureState::Stopping);
        }
        let result = self.finish().map(|(result, _)| Box::new(result));
        let mut inner = self.lock();
        Self::set_state(
            &mut inner,
            &CaptureState::Failed {
                error: CaptureError::WriterFailed(error.to_string()),
                recording: result,
            },
        );
    }
}
