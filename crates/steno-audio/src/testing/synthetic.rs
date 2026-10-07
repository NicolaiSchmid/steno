//! A [`CaptureBackend`] without the HAL.
//! Swift: `Sources/StenoAudio/Testing/SyntheticCaptureBackend.swift`.
//!
//! Deterministic sines per lane (integer phase accumulators, identical on
//! every machine), delivered from a producer thread through the same
//! [`LaneFrameSink`] protocol the IOProc uses, in callbacks of
//! `callback_frames`. By default it runs as fast as the rings accept (a
//! 30 s recording takes milliseconds); `real_time` paces it at wall-clock
//! speed for the CLI.
//!
//! Device changes, so the session's rebuild runs on CI exactly as in
//! production: `change_device_after` reports `DefaultInputChanged` after
//! exactly that many seconds of one `start` and ends the producer thread,
//! like a microphone that moved; it fires `changes` times per backend
//! instance (one by default), not once per `start`, or the rebuilt backend
//! would report again and loop. `restarts_that_fail` makes that many
//! `start` calls after the first fail with `InputDeviceUnavailable`, the
//! device still absent; `stream` is what the first start reports and
//! `stream_after_restart` what every restart reports (new latencies, a
//! microphone standing in), `SYNTHETIC` when `None`. `seconds` counts per
//! `start`, so a restarted backend delivers again, and `frames_delivered`
//! sums over starts.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_core::AudioLane;

use super::fixtures::AudioFixtures;
use crate::SAMPLE_RATE;
use crate::capture::{CaptureBackend, CaptureError, CaptureStream, DeviceChangeReason};
use crate::realtime::LaneFrameSink;

/// A delayed, attenuated copy of another lane (the loudspeaker echo the mic
/// hears in a call).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntheticEcho {
    /// The lane echoed.
    pub of: AudioLane,
    /// Seconds behind it.
    pub delay: f64,
    /// Linear gain; 0.5 is -6 dB.
    pub gain: f64,
}

/// One synthetic lane: a sine, optionally plus an echo of another lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntheticLane {
    /// Hertz; 0 at amplitude 0 is silence.
    pub frequency: f64,
    /// Linear peak.
    pub amplitude: f64,
    /// An echo of another lane mixed in.
    pub echo: Option<SyntheticEcho>,
}

impl SyntheticLane {
    /// No signal.
    pub const SILENCE: SyntheticLane = SyntheticLane {
        frequency: 0.0,
        amplitude: 0.0,
        echo: None,
    };

    /// A tone at amplitude 0.5.
    #[must_use]
    pub fn tone(frequency: f64) -> Self {
        Self {
            frequency,
            amplitude: 0.5,
            echo: None,
        }
    }

    /// A tone at `amplitude`.
    #[must_use]
    pub fn new(frequency: f64, amplitude: f64) -> Self {
        Self {
            frequency,
            amplitude,
            echo: None,
        }
    }

    /// Adds an echo of lane `of`, `delay` seconds behind, at `gain`.
    #[must_use]
    pub fn with_echo(mut self, of: AudioLane, delay: f64, gain: f64) -> Self {
        self.echo = Some(SyntheticEcho { of, delay, gain });
        self
    }
}

/// The knobs of [`SyntheticCaptureBackend::new`].
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticOptions {
    /// What each lane carries.
    pub signals: BTreeMap<AudioLane, SyntheticLane>,
    /// How long one `start` delivers before the producer ends.
    pub seconds: f64,
    /// Frames per callback, the HAL's buffer size.
    pub callback_frames: usize,
    /// Pace the callbacks on the wall clock instead of as fast as possible.
    pub real_time: bool,
    /// Seconds into a start at which a device change is reported.
    pub change_device_after: Option<f64>,
    /// How many starts report one.
    pub changes: usize,
    /// How many restarts after a change fail before one succeeds.
    pub restarts_that_fail: usize,
    /// The stream the first start reports, when it should differ.
    pub stream: Option<CaptureStream>,
    /// The stream the restarted backend reports, when it should differ.
    pub stream_after_restart: Option<CaptureStream>,
}

impl SyntheticOptions {
    /// The plan's spelling: one tone per lane at amplitude 0.5, silence for
    /// a lane without a tone.
    #[must_use]
    pub fn tones(lanes: &[AudioLane], tones: &[(AudioLane, f64)], seconds: f64) -> Self {
        let signals = lanes
            .iter()
            .map(|lane| {
                let tone = tones
                    .iter()
                    .find(|(l, _)| l == lane)
                    .map_or(SyntheticLane::SILENCE, |(_, f)| SyntheticLane::tone(*f));
                (*lane, tone)
            })
            .collect();
        Self::signals(signals, seconds)
    }

    /// `signals` for `seconds`: 512-frame callbacks, as fast as possible, no
    /// device changes.
    #[must_use]
    pub fn signals(signals: BTreeMap<AudioLane, SyntheticLane>, seconds: f64) -> Self {
        Self {
            signals,
            seconds,
            callback_frames: 512,
            real_time: false,
            change_device_after: None,
            changes: 1,
            restarts_that_fail: 0,
            stream: None,
            stream_after_restart: None,
        }
    }

    /// Frames per callback.
    #[must_use]
    pub fn callback_frames(mut self, frames: usize) -> Self {
        self.callback_frames = frames;
        self
    }

    /// Report a device change this many seconds into a start.
    #[must_use]
    pub fn change_device_after(mut self, seconds: f64) -> Self {
        self.change_device_after = Some(seconds);
        self
    }

    /// How many starts report a change.
    #[must_use]
    pub fn changes(mut self, count: usize) -> Self {
        self.changes = count;
        self
    }

    /// Fail this many restarts first.
    #[must_use]
    pub fn restarts_that_fail(mut self, count: usize) -> Self {
        self.restarts_that_fail = count;
        self
    }

    /// What the first start reports.
    #[must_use]
    pub fn stream(mut self, stream: CaptureStream) -> Self {
        self.stream = Some(stream);
        self
    }

    /// What the restarted backend reports.
    #[must_use]
    pub fn stream_after_restart(mut self, stream: CaptureStream) -> Self {
        self.stream_after_restart = Some(stream);
        self
    }

    /// Pace on the wall clock.
    #[must_use]
    pub fn real_time(mut self, real_time: bool) -> Self {
        self.real_time = real_time;
        self
    }
}

/// The test double for the live backend; see the module doc.
pub struct SyntheticCaptureBackend {
    options: SyntheticOptions,
    state: Mutex<State>,
    /// Spent by the producer thread when a change fires, as in Swift, so a
    /// start stopped before its change keeps the change for the next one.
    changes_remaining: Arc<AtomicUsize>,
    stop_requested: Arc<AtomicBool>,
    frames_delivered: Arc<AtomicUsize>,
    completion: Arc<Completion>,
}

struct State {
    thread: Option<JoinHandle<()>>,
    failing_restarts_remaining: usize,
    start_count: usize,
}

impl std::fmt::Debug for SyntheticCaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntheticCaptureBackend")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl SyntheticCaptureBackend {
    /// Built from `options`; nothing runs until `start`.
    #[must_use]
    pub fn new(options: SyntheticOptions) -> Self {
        Self {
            state: Mutex::new(State {
                thread: None,
                failing_restarts_remaining: options.restarts_that_fail,
                start_count: 0,
            }),
            changes_remaining: Arc::new(AtomicUsize::new(options.changes)),
            options,
            stop_requested: Arc::new(AtomicBool::new(false)),
            frames_delivered: Arc::new(AtomicUsize::new(0)),
            completion: Arc::new(Completion::default()),
        }
    }

    /// One tone per lane at amplitude 0.5 for `seconds`.
    #[must_use]
    pub fn tones(lanes: &[AudioLane], tones: &[(AudioLane, f64)], seconds: f64) -> Self {
        Self::new(SyntheticOptions::tones(lanes, tones, seconds))
    }

    /// The options it was built with.
    #[must_use]
    pub fn options(&self) -> &SyntheticOptions {
        &self.options
    }

    /// Frames delivered to the sink over every `start` so far (including
    /// refused callbacks).
    #[must_use]
    pub fn frames_delivered(&self) -> usize {
        self.frames_delivered.load(Ordering::Relaxed)
    }

    /// `start` calls so far, the failed ones included.
    #[must_use]
    pub fn starts(&self) -> usize {
        self.lock().start_count
    }

    /// Blocks until the producer thread of the latest `start` has delivered
    /// `seconds` of audio, reported a device change or been stopped. Tests
    /// wait on this instead of wall time.
    pub fn wait_until_finished(&self) {
        self.completion.wait();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CaptureBackend for SyntheticCaptureBackend {
    fn start(
        &self,
        lanes: &[AudioLane],
        _input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let mut state = self.lock();
        if state.thread.is_some() {
            return Err(CaptureError::InvalidState(
                "synthetic backend already started".into(),
            ));
        }
        let is_restart = state.start_count > 0;
        state.start_count += 1;
        if is_restart && state.failing_restarts_remaining > 0 {
            state.failing_restarts_remaining -= 1;
            return Err(CaptureError::InputDeviceUnavailable);
        }
        self.stop_requested.store(false, Ordering::Release);
        self.completion.reset();
        let mut generator =
            Generator::new(lanes, &self.options.signals, self.options.callback_frames);
        // Whole frames of positive durations.
        let total_frames = (self.options.seconds * SAMPLE_RATE) as usize;
        let change_frame = if self.changes_remaining.load(Ordering::Relaxed) > 0 {
            self.options
                .change_device_after
                .map(|s| (s * SAMPLE_RATE) as usize)
        } else {
            None
        };
        let changes_remaining = Arc::clone(&self.changes_remaining);
        let real_time = self.options.real_time;
        let callback_frames = self.options.callback_frames;
        let stop = Arc::clone(&self.stop_requested);
        let delivered_counter = Arc::clone(&self.frames_delivered);
        let completion = Arc::clone(&self.completion);
        let lane_count = lanes.len();
        let delivered_before = delivered_counter.load(Ordering::Relaxed);
        let thread = std::thread::Builder::new()
            .name("steno-synth".into())
            .spawn(move || {
                let mut delivered = 0usize;
                let started = Instant::now();
                while delivered < total_frames && !stop.load(Ordering::Acquire) {
                    if let Some(change) = change_frame
                        && delivered >= change
                    {
                        changes_remaining.fetch_sub(1, Ordering::Relaxed);
                        sink.report_device_change(DeviceChangeReason::DefaultInputChanged);
                        break;
                    }
                    // The callback before a change is clipped to it, so the
                    // change lands on the exact frame and a test can count
                    // what each start delivered.
                    let mut frames = callback_frames.min(total_frames - delivered);
                    if let Some(change) = change_frame {
                        frames = frames.min(change - delivered);
                    }
                    if real_time {
                        // Positive sample counts in seconds.
                        let due = started + Duration::from_secs_f64(delivered as f64 / SAMPLE_RATE);
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        }
                    } else {
                        let mut spins = 0;
                        while !sink.rings().has_room(frames) && !stop.load(Ordering::Acquire) {
                            spins += 1;
                            std::thread::sleep(Duration::from_micros(if spins < 100 {
                                200
                            } else {
                                2_000
                            }));
                        }
                    }
                    generator.fill(frames);
                    if sink.begin_callback(frames) {
                        for lane in 0..lane_count {
                            sink.write_slice(lane, generator.buffer(lane));
                        }
                        sink.end_callback();
                    }
                    delivered += frames;
                    delivered_counter.store(delivered_before + delivered, Ordering::Relaxed);
                }
                completion.finish();
            })
            .expect("spawn synthetic producer");
        state.thread = Some(thread);
        let stream = if is_restart {
            &self.options.stream_after_restart
        } else {
            &self.options.stream
        };
        Ok(stream.clone().unwrap_or(CaptureStream::SYNTHETIC))
    }

    fn stop(&self) {
        let thread = self.lock().thread.take();
        if let Some(thread) = thread {
            self.stop_requested.store(true, Ordering::Release);
            let _ = thread.join();
        }
    }
}

/// One-shot completion any number of threads can wait on.
#[derive(Debug, Default)]
struct Completion {
    finished: Mutex<bool>,
    condvar: Condvar,
}

impl Completion {
    fn reset(&self) {
        *self.lock() = false;
    }

    fn finish(&self) {
        *self.lock() = true;
        self.condvar.notify_all();
    }

    fn wait(&self) {
        let mut finished = self.lock();
        while !*finished {
            finished = self
                .condvar
                .wait(finished)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
        self.finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Per-lane phase accumulators and preallocated output buffers.
struct Generator {
    states: Vec<LaneState>,
    /// Every lane's phase at the start of the current fill, so an echo of a
    /// lane already advanced in this fill still reads the right phase.
    start_phases: Vec<u32>,
    buffers: Vec<Vec<f32>>,
    position: usize,
}

#[derive(Clone, Copy)]
struct LaneState {
    increment: u32,
    phase: u32,
    amplitude: f32,
    echo_lane: Option<usize>,
    echo_delay_samples: usize,
    echo_gain: f32,
}

impl Generator {
    fn new(
        lanes: &[AudioLane],
        signals: &BTreeMap<AudioLane, SyntheticLane>,
        callback_frames: usize,
    ) -> Self {
        let states = lanes
            .iter()
            .map(|lane| {
                let signal = signals.get(lane).copied().unwrap_or(SyntheticLane::SILENCE);
                let echo_lane = signal
                    .echo
                    .and_then(|echo| lanes.iter().position(|l| *l == echo.of));
                // Positive, rounded as Swift rounds.
                LaneState {
                    increment: AudioFixtures::phase_increment(signal.frequency),
                    phase: 0,
                    amplitude: signal.amplitude as f32,
                    echo_lane,
                    echo_delay_samples: (signal.echo.map_or(0.0, |e| e.delay) * SAMPLE_RATE).round()
                        as usize,
                    echo_gain: signal.echo.map_or(0.0, |e| e.gain) as f32,
                }
            })
            .collect();
        Self {
            states,
            start_phases: vec![0; lanes.len()],
            buffers: lanes.iter().map(|_| vec![0.0; callback_frames]).collect(),
            position: 0,
        }
    }

    fn buffer(&self, lane: usize) -> &[f32] {
        &self.buffers[lane]
    }

    /// The fixtures' integer-phase sine, in `f32` as the lanes are.
    #[inline(always)]
    fn sine(phase: u32) -> f32 {
        AudioFixtures::sine(phase) as f32
    }

    /// Writes `frames` samples per lane. Echo terms are the source lane's
    /// sine at a phase offset, silent until the delay has elapsed.
    fn fill(&mut self, frames: usize) {
        for (lane, state) in self.states.iter().enumerate() {
            self.start_phases[lane] = state.phase;
        }
        for lane in 0..self.states.len() {
            let mut state = self.states[lane];
            for index in 0..frames {
                let mut value = state.amplitude * Self::sine(state.phase);
                if let Some(echo_lane) = state.echo_lane
                    && self.position + index >= state.echo_delay_samples
                {
                    let source = self.states[echo_lane];
                    // Wrapping phase arithmetic, as the accumulator itself.
                    let source_phase = self.start_phases[echo_lane]
                        .wrapping_add((index as u32).wrapping_mul(source.increment))
                        .wrapping_sub(
                            (state.echo_delay_samples as u32).wrapping_mul(source.increment),
                        );
                    value += state.echo_gain * source.amplitude * Self::sine(source_phase);
                }
                self.buffers[lane][index] = value;
                state.phase = state.phase.wrapping_add(state.increment);
            }
            self.states[lane] = state;
        }
        self.position += frames;
    }
}
