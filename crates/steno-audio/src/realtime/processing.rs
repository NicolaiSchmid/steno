//! Drains the sink's rings in 10 ms frames on a dedicated thread.
//! Swift: `Sources/StenoAudio/RealTime/ProcessingThread.swift`.
//!
//! Echo cancellation on the mic lane with the system lane of the same frame
//! as far-end (optionally delayed by the device latency through a
//! preallocated line), metering, and the hand-off to the writer through
//! [`FrameRelay`]. Every buffer is allocated in `new`; the loop allocates
//! nothing, takes no locks and never blocks on the writer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use steno_core::{AudioLane, EchoCanceller};

use super::level_meter::{LevelMeter, LevelSlot};
use super::relay::FrameRelay;
use super::ring::LaneRingBuffer;
use super::sink::LaneFrameSink;
use crate::FRAME_SIZE;

pub struct ProcessingConfiguration {
    pub lanes: Vec<AudioLane>,
    pub frame_size: usize,
    pub echo_canceller: Option<Box<dyn EchoCanceller>>,
    /// Samples the far-end is delayed by before cancellation (0: none).
    pub far_end_delay_frames: usize,
    pub keep_raw_mic: bool,
    /// Frames per level publish: 10 frames is 100 ms, 10 Hz.
    pub frames_per_level: usize,
}

impl ProcessingConfiguration {
    #[must_use]
    pub fn new(lanes: &[AudioLane], echo_canceller: Option<Box<dyn EchoCanceller>>) -> Self {
        Self {
            lanes: lanes.to_vec(),
            frame_size: FRAME_SIZE,
            echo_canceller,
            far_end_delay_frames: 0,
            keep_raw_mic: false,
            frames_per_level: 10,
        }
    }
}

/// The per-frame state, owned by the loop (or by the test calling `drain`).
struct Worker {
    sink: Arc<LaneFrameSink>,
    relay: Arc<FrameRelay>,
    frame_size: usize,
    frames_per_level: usize,
    keep_raw_mic: bool,
    mic_index: Option<usize>,
    system_index: Option<usize>,
    /// What the rings deliver, one buffer per lane.
    lane_buffers: Vec<Vec<f32>>,
    /// The canceller's output; the written mic channel while AEC runs.
    processed_mic: Vec<f32>,
    delayed_far: Vec<f32>,
    delay_line: Option<LaneRingBuffer>,
    aec: Option<Box<dyn EchoCanceller>>,
    meters: Vec<LevelMeter>,
    frames_since_publish: usize,
    shared: Arc<Shared>,
}

struct Shared {
    levels: Arc<LevelSlot>,
    frames_processed: AtomicUsize,
    system_peak_bits: AtomicU32,
    stop_requested: AtomicBool,
}

impl Worker {
    /// Processes every whole frame the rings hold right now.
    #[inline(always)]
    fn drain(&mut self) {
        let frame_size = self.frame_size;
        while self.sink.available_to_read() >= frame_size {
            for (index, buffer) in self.lane_buffers.iter_mut().enumerate() {
                self.sink.ring(index).read(&mut buffer[..frame_size]);
            }
            self.process_frame();
        }
    }

    #[inline(always)]
    fn process_frame(&mut self) {
        let frame_size = self.frame_size;
        let aec_indices = match (&self.aec, self.mic_index, self.system_index) {
            (Some(_), Some(mic), Some(system)) => Some((mic, system)),
            _ => None,
        };
        if let (Some(aec), Some((mic, system))) = (self.aec.as_mut(), aec_indices) {
            let system_lane = &self.lane_buffers[system][..frame_size];
            let far_end: &[f32] = match &self.delay_line {
                Some(line) => {
                    line.write_slice(system_lane);
                    line.read(&mut self.delayed_far[..frame_size]);
                    &self.delayed_far[..frame_size]
                }
                None => system_lane,
            };
            aec.process(
                &self.lane_buffers[mic][..frame_size],
                far_end,
                &mut self.processed_mic[..frame_size],
            );
        }

        // Metering on what is written.
        for (index, meter) in self.meters.iter_mut().enumerate() {
            meter.accumulate(output_of(
                &self.lane_buffers,
                &self.processed_mic,
                frame_size,
                index,
                aec_indices,
            ));
        }
        if let Some(system) = self.system_index {
            let peak = self.meters[system].linear_peak();
            if peak > self.shared.system_peak() {
                self.shared
                    .system_peak_bits
                    .store(peak.to_bits(), Ordering::Relaxed);
            }
        }
        self.frames_since_publish += 1;
        if self.frames_since_publish >= self.frames_per_level {
            self.frames_since_publish = 0;
            // In person the single `mixed` lane is what the meter calls "mic".
            let mic_level = self.meters[self.mic_index.unwrap_or(0)].current();
            let system_level = self.system_index.map(|index| self.meters[index].current());
            self.shared.levels.publish(mic_level, system_level);
            for meter in &mut self.meters {
                meter.flush();
            }
        }

        // Hand-off; refused frames are counted by the relay.
        self.shared.frames_processed.fetch_add(1, Ordering::Relaxed);
        if !self.relay.begin_frame() {
            return;
        }
        for index in 0..self.lane_buffers.len() {
            self.relay.write(index, self.output(index, aec_indices));
        }
        if let (true, Some(mic)) = (self.keep_raw_mic, self.mic_index) {
            self.relay.write(
                self.lane_buffers.len(),
                &self.lane_buffers[mic][..frame_size],
            );
        }
        self.relay.end_frame();
    }

    #[inline(always)]
    fn output(&self, index: usize, aec: Option<(usize, usize)>) -> &[f32] {
        output_of(
            &self.lane_buffers,
            &self.processed_mic,
            self.frame_size,
            index,
            aec,
        )
    }
}

/// What is metered and written for `index`: the lane buffer, except that
/// the mic lane is the canceller's output while AEC runs.
#[inline(always)]
fn output_of<'a>(
    lane_buffers: &'a [Vec<f32>],
    processed_mic: &'a [f32],
    frame_size: usize,
    index: usize,
    aec: Option<(usize, usize)>,
) -> &'a [f32] {
    match aec {
        Some((mic, _)) if mic == index => &processed_mic[..frame_size],
        _ => &lane_buffers[index][..frame_size],
    }
}

impl Shared {
    fn system_peak(&self) -> f32 {
        f32::from_bits(self.system_peak_bits.load(Ordering::Relaxed))
    }
}

/// The handle the session holds. `new` allocates everything; `start` spawns
/// the loop; `stop` joins it and hands the echo canceller back so the
/// session can `reset` it and give it to the next thread after a device
/// change.
pub struct ProcessingThread {
    worker: Option<Worker>,
    thread: Option<JoinHandle<Worker>>,
    shared: Arc<Shared>,
    /// The sink's wake, so `stop` can rouse a parked loop at once.
    sink: Arc<LaneFrameSink>,
}

impl ProcessingThread {
    /// `levels` `None` allocates a fresh slot; the session passes the
    /// previous thread's slot when it rebuilds after a device change, so
    /// the writer thread keeps reading the one it was given at start.
    #[must_use]
    pub fn new(
        sink: Arc<LaneFrameSink>,
        relay: Arc<FrameRelay>,
        configuration: ProcessingConfiguration,
        levels: Option<Arc<LevelSlot>>,
    ) -> Self {
        let frame_size = configuration.frame_size;
        let mic_index = configuration
            .lanes
            .iter()
            .position(|l| *l == AudioLane::Mic);
        let system_index = configuration
            .lanes
            .iter()
            .position(|l| *l == AudioLane::System);
        let aec = match (configuration.echo_canceller, mic_index, system_index) {
            (Some(canceller), Some(_), Some(_)) => Some(canceller),
            // A canceller without both lanes is kept so the session gets it
            // back, but never runs.
            (canceller, _, _) => canceller,
        };
        let runs_aec = aec.is_some() && mic_index.is_some() && system_index.is_some();
        let delay_line = if runs_aec && configuration.far_end_delay_frames > 0 {
            let line = LaneRingBuffer::new(configuration.far_end_delay_frames + frame_size);
            line.write_zeros(configuration.far_end_delay_frames);
            Some(line)
        } else {
            None
        };
        let shared = Arc::new(Shared {
            levels: levels.unwrap_or_else(|| Arc::new(LevelSlot::new(system_index.is_some()))),
            frames_processed: AtomicUsize::new(0),
            system_peak_bits: AtomicU32::new(0),
            stop_requested: AtomicBool::new(false),
        });
        let worker = Worker {
            sink: Arc::clone(&sink),
            relay,
            frame_size,
            frames_per_level: configuration.frames_per_level,
            keep_raw_mic: configuration.keep_raw_mic,
            mic_index,
            system_index,
            lane_buffers: configuration
                .lanes
                .iter()
                .map(|_| vec![0.0; frame_size])
                .collect(),
            processed_mic: vec![0.0; frame_size],
            delayed_far: vec![0.0; frame_size],
            delay_line,
            aec,
            meters: configuration
                .lanes
                .iter()
                .map(|_| LevelMeter::new())
                .collect(),
            frames_since_publish: 0,
            shared: Arc::clone(&shared),
        };
        Self {
            worker: Some(worker),
            thread: None,
            shared,
            sink,
        }
    }

    #[must_use]
    pub fn levels(&self) -> &Arc<LevelSlot> {
        &self.shared.levels
    }

    /// Frames handed to the relay or refused by it.
    #[must_use]
    pub fn frames_processed(&self) -> usize {
        self.shared.frames_processed.load(Ordering::Relaxed)
    }

    /// The loudest system-lane sample so far, linear.
    #[must_use]
    pub fn system_peak(&self) -> f32 {
        self.shared.system_peak()
    }

    pub fn start(&mut self) {
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        shared.stop_requested.store(false, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("steno-process".into())
            .spawn(move || {
                while !shared.stop_requested.load(Ordering::Acquire) {
                    worker.sink.wake().wait(Duration::from_millis(20));
                    worker.drain();
                }
                worker.drain();
                worker
            })
            .expect("spawn processing thread");
        self.thread = Some(handle);
    }

    /// Asks the loop to stop, waits for it to drain every whole frame left
    /// in the rings and exit. `frames_processed` and `system_peak` stay
    /// readable afterwards.
    pub fn stop(&mut self) {
        self.join();
    }

    /// The echo canceller the thread was given, once stopped (or never
    /// started), so the session can `reset` it and hand it to the next
    /// thread after a device change. `None` while the loop runs.
    pub fn take_echo_canceller(&mut self) -> Option<Box<dyn EchoCanceller>> {
        self.worker.as_mut().and_then(|worker| worker.aec.take())
    }

    fn join(&mut self) {
        if let Some(handle) = self.thread.take() {
            self.shared.stop_requested.store(true, Ordering::Release);
            self.sink.wake().signal();
            if let Ok(worker) = handle.join() {
                self.worker = Some(worker);
            }
        }
    }

    /// Runs the loop body on the calling thread (the real-time allocation
    /// test, which must own the thread the hook watches). Only before
    /// `start`.
    pub fn drain_on_caller(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.drain();
        }
    }
}

impl Drop for ProcessingThread {
    fn drop(&mut self) {
        self.join();
    }
}
