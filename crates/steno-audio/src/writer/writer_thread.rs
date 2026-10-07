//! The serial writer: drains [`FrameRelay`] on its own thread, hands each
//! frame to the [`RecordingWriting`] implementation, and republishes the
//! processing thread's levels whenever their generation changed (so the
//! 10 Hz level stream costs the processing thread nothing).
//! Swift: `Sources/StenoAudio/Writer/WriterThread.swift`.
//!
//! Every [`SYNC_INTERVAL_FRAMES`] frames the thread
//! [`sync`](RecordingWriting::sync)s the master, so a power loss or a kernel
//! crash loses at most the last 5 s of a recording, not everything still in
//! the page cache. Rust only: Swift synced at the close alone.
//!
//! A write or sync error is kept (for [`WriterThread::take_error`]),
//! reported once and stops further writes; the loop keeps draining so the
//! relay never fills. The lane slices handed to the writer sit in a stack
//! array sized by [`AudioLane::ALL`], so a drained frame allocates nothing (not a
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

/// Frames written between two syncs of the master: 5 s of 10 ms frames,
/// counted in audio rather than wall time, so gap silence counts and a
/// test needs no clock. A slow sync is covered by the relay's 20 s
/// ([`CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES`]).
///
/// [`CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES`]: crate::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES
pub const SYNC_INTERVAL_FRAMES: usize = 500;

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
    /// Frames written since the last sync.
    unsynced_frames: usize,
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
            let written = self.writer.write(&frames).and_then(|()| {
                self.unsynced_frames += 1;
                if self.unsynced_frames < SYNC_INTERVAL_FRAMES {
                    return Ok(());
                }
                self.unsynced_frames = 0;
                self.writer.sync()
            });
            if let Err(error) = written {
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
            unsynced_frames: 0,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::FRAME_SIZE;
    use crate::writer::{LaneFrames, RecordingFiles};

    /// What the fake writer was asked to do, in order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Write,
        Sync,
    }

    /// Records every call; the sync numbered `fail_sync` (from 1) fails.
    struct Recording {
        calls: Arc<Mutex<Vec<Call>>>,
        fail_sync: Option<usize>,
    }

    impl RecordingWriting for Recording {
        fn files(&self) -> RecordingFiles {
            RecordingFiles {
                master: "master.caf".into(),
                sidecars_16k: std::collections::BTreeMap::new(),
                raw_mic: None,
                duration: 0.0,
            }
        }
        fn write(&mut self, _frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().push(Call::Write);
            Ok(())
        }
        fn sync(&mut self) -> Result<(), CaptureError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call::Sync);
            let syncs = calls.iter().filter(|call| **call == Call::Sync).count();
            if self.fail_sync == Some(syncs) {
                return Err(CaptureError::WriterFailed("EIO".into()));
            }
            Ok(())
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            Ok(self.files())
        }
    }

    /// Pushes `frames` one-lane frames through a writer thread over the
    /// fake and returns its calls and the errors it reported. The relay
    /// holds them all, so none is dropped.
    fn run(frames: usize, fail_sync: Option<usize>) -> (Vec<Call>, Vec<CaptureError>) {
        let relay = Arc::new(FrameRelay::new(1, FRAME_SIZE, frames));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let mut thread = WriterThread::new(
            Arc::clone(&relay),
            Box::new(Recording {
                calls: Arc::clone(&calls),
                fail_sync,
            }),
            Arc::new(LevelSlot::new(false)),
            1,
            false,
            Box::new(|_| {}),
            Box::new({
                let errors = Arc::clone(&errors);
                move |error| errors.lock().unwrap().push(error)
            }),
        );
        let zeros = vec![0.0f32; FRAME_SIZE];
        for _ in 0..frames {
            assert!(relay.begin_frame());
            relay.write(0, &zeros);
            relay.end_frame();
        }
        thread.start();
        thread.stop();
        let calls = calls.lock().unwrap().clone();
        let errors = errors.lock().unwrap().clone();
        (calls, errors)
    }

    #[test]
    fn the_master_is_synced_after_every_interval_of_frames_and_not_before() {
        let (calls, errors) = run(SYNC_INTERVAL_FRAMES * 3 + 10, None);
        assert!(errors.is_empty(), "{errors:?}");
        let syncs: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| **call == Call::Sync)
            .map(|(index, _)| index)
            .collect();
        // Each sync follows the interval's last write: after writes
        // 500, 1000 and 1500, with the syncs before them counted in.
        assert_eq!(
            syncs,
            [
                SYNC_INTERVAL_FRAMES,
                2 * SYNC_INTERVAL_FRAMES + 1,
                3 * SYNC_INTERVAL_FRAMES + 2
            ]
        );
    }

    #[test]
    fn a_failed_sync_is_reported_once_and_stops_the_writes() {
        let (calls, errors) = run(SYNC_INTERVAL_FRAMES * 3, Some(1));
        assert_eq!(errors, [CaptureError::WriterFailed("EIO".into())]);
        assert_eq!(
            calls.iter().filter(|call| **call == Call::Write).count(),
            SYNC_INTERVAL_FRAMES,
            "no write after the failed sync"
        );
        assert_eq!(calls.last(), Some(&Call::Sync));
    }
}
