//! The serial writer: drains [`FrameRelay`] on its own thread, hands each
//! frame to the [`RecordingWriting`] implementation, and republishes the
//! processing thread's levels whenever their generation changed (so the
//! 10 Hz level stream costs the processing thread nothing).
//! Swift: `Sources/StenoAudio/Writer/WriterThread.swift`.
//!
//! A write error is kept (for [`WriterThread::take_error`]), reported once
//! and stops further writes; the loop keeps draining so the relay never
//! fills. The lane slices handed to the writer sit in a stack array sized
//! by [`AudioLane::ALL`], so a drained frame allocates nothing (not a
//! real-time requirement here, the thread does file I/O, but one less
//! allocation per 10 ms).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use steno_core::AudioLane;

use super::recording_writer::{LaneFrames, RecordingWriting};
use crate::capture::{CaptureError, LaneLevels};
use crate::realtime::{FrameRelay, LevelSlot};

/// Runs on the writer thread with the levels it republishes.
pub type LevelsHandler = Box<dyn Fn(LaneLevels) + Send>;
/// Runs on the writer thread once, with the first write error.
pub type ErrorHandler = Box<dyn Fn(CaptureError) + Send>;

/// A recording has at most one channel per [`AudioLane`].
const MAX_LANES: usize = AudioLane::ALL.len();

struct Worker {
    relay: Arc<FrameRelay>,
    writer: Box<dyn RecordingWriting>,
    levels: Arc<LevelSlot>,
    buffers: Vec<Vec<f32>>,
    lane_count: usize,
    has_raw_mic: bool,
    on_levels: LevelsHandler,
    on_error: ErrorHandler,
    failed: Arc<AtomicBool>,
    /// The first write error, kept for whoever closes the files.
    error: Option<CaptureError>,
    last_generation: usize,
}

impl Worker {
    fn drain(&mut self) {
        while self.relay.available_frames() > 0 {
            for (channel, buffer) in self.buffers.iter_mut().enumerate() {
                self.relay.read(channel, buffer);
            }
            if self.failed.load(Ordering::Acquire) {
                continue;
            }
            let mut lanes: [&[f32]; MAX_LANES] = [&[]; MAX_LANES];
            for (slot, buffer) in lanes.iter_mut().zip(&self.buffers[..self.lane_count]) {
                *slot = buffer.as_slice();
            }
            let frames = LaneFrames {
                frame_count: self.relay.frame_size(),
                lanes: &lanes[..self.lane_count],
                raw_mic: self
                    .has_raw_mic
                    .then(|| self.buffers[self.lane_count].as_slice()),
            };
            if let Err(error) = self.writer.write(&frames) {
                self.failed.store(true, Ordering::Release);
                self.error = Some(error.clone());
                (self.on_error)(error);
            }
        }
    }

    fn publish_levels_if_changed(&mut self) {
        let generation = self.levels.current_generation();
        if generation == self.last_generation {
            return;
        }
        self.last_generation = generation;
        (self.on_levels)(self.levels.levels());
    }
}

/// The writer thread's handle; see the module doc.
pub struct WriterThread {
    worker: Option<Worker>,
    thread: Option<JoinHandle<Worker>>,
    stop_requested: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    relay: Arc<FrameRelay>,
}

impl WriterThread {
    /// Builds the worker over `relay`; `start` spawns it.
    #[must_use]
    pub fn new(
        relay: Arc<FrameRelay>,
        writer: Box<dyn RecordingWriting>,
        levels: Arc<LevelSlot>,
        lane_count: usize,
        has_raw_mic: bool,
        on_levels: LevelsHandler,
        on_error: ErrorHandler,
    ) -> Self {
        assert!(
            lane_count <= MAX_LANES,
            "{lane_count} lanes, at most {MAX_LANES} exist"
        );
        let failed = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            relay: Arc::clone(&relay),
            writer,
            levels,
            buffers: (0..relay.channels())
                .map(|_| vec![0.0; relay.frame_size()])
                .collect(),
            lane_count,
            has_raw_mic,
            on_levels,
            on_error,
            failed: Arc::clone(&failed),
            error: None,
            last_generation: 0,
        };
        Self {
            worker: Some(worker),
            thread: None,
            stop_requested: Arc::new(AtomicBool::new(false)),
            failed,
            relay,
        }
    }

    /// A write has failed; frames are drained and dropped from now on.
    #[must_use]
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Spawns the thread; a second call does nothing.
    pub fn start(&mut self) {
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        let stop = Arc::clone(&self.stop_requested);
        stop.store(false, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("steno-writer".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    worker.relay.wake().wait(Duration::from_millis(50));
                    worker.drain();
                    worker.publish_levels_if_changed();
                }
                worker.drain();
                worker.publish_levels_if_changed();
                worker
            })
            .expect("spawn writer thread");
        self.thread = Some(handle);
    }

    /// Drains everything left in the relay, then returns.
    pub fn stop(&mut self) {
        self.join();
    }

    /// The first write error, once the thread is stopped; `None` while the
    /// loop runs, after the writer was taken, or when every write succeeded.
    pub fn take_error(&mut self) -> Option<CaptureError> {
        self.worker.as_mut().and_then(|worker| worker.error.take())
    }

    /// The writer, once the thread is stopped (or was never started), so
    /// the session can close its files. `None` while the loop runs.
    pub fn take_writer(&mut self) -> Option<Box<dyn RecordingWriting>> {
        self.worker.take().map(|worker| worker.writer)
    }

    fn join(&mut self) {
        if let Some(handle) = self.thread.take() {
            self.stop_requested.store(true, Ordering::Release);
            self.relay.wake().signal();
            if let Ok(worker) = handle.join() {
                self.worker = Some(worker);
            }
        }
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        self.join();
    }
}
