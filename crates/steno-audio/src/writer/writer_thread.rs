//! The serial writer: drains [`FrameRelay`] on its own thread, hands each
//! frame to the [`RecordingWriting`] implementation, and republishes the
//! processing thread's levels whenever their generation changed (so the
//! 10 Hz level stream costs the processing thread nothing).
//! Swift: `Sources/StenoAudio/Writer/WriterThread.swift`.
//!
//! Every [`SYNC_INTERVAL_FRAMES`] frames the thread
//! [`sync`](RecordingWriting::sync)s the master (`File::sync_data`,
//! `F_FULLFSYNC` on the Mac, a plain `fsync` where a filesystem refuses
//! that), so a power loss or a kernel crash loses about the last 5 s of a
//! recording, more while the writer is behind, not everything still in the
//! page cache. A sync that fails even so is logged once, kept for
//! [`WriterThread::take_error`] (what was written may not be on disk) and
//! tried again at the next interval; the writes go on. Rust only: Swift
//! synced at the close alone.
//!
//! A write error is kept (for [`WriterThread::take_error`], in place of a
//! failed sync's), reported once and stops further writes; the loop keeps
//! draining so the relay never fills. The lane slices handed to the writer
//! sit in a stack array sized by [`AudioLane::ALL`], so a drained frame
//! allocates nothing (not a real-time requirement here, the thread does
//! file I/O, but one less allocation per 10 ms).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use steno_core::AudioLane;

use super::io_error;
use super::recording_writer::{LaneFrames, RecordingWriting};
use crate::FRAMES_PER_SECOND;
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
pub const SYNC_INTERVAL_FRAMES: usize = 5 * FRAMES_PER_SECOND;

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
    /// The write error, else the first failed sync's, kept for whoever
    /// closes the files.
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
            match self.writer.write(&frames) {
                Ok(()) => self.sync_if_due(),
                Err(error) => {
                    self.failed.store(true, Ordering::Release);
                    self.error = Some(error.clone());
                    (self.on_error)(error);
                }
            }
        }
    }

    /// Syncs the master after every [`SYNC_INTERVAL_FRAMES`] frames
    /// written. The first failure is kept and logged by its kind and OS
    /// code alone (the log's privacy rule: no path); later ones are not,
    /// and the next interval tries again. Only a sync after a successful
    /// write gets here, so `error` holds nothing but a failed sync's.
    fn sync_if_due(&mut self) {
        self.unsynced_frames += 1;
        if self.unsynced_frames < SYNC_INTERVAL_FRAMES {
            return;
        }
        self.unsynced_frames = 0;
        if let Err(error) = self.writer.sync()
            && self.error.is_none()
        {
            tracing::warn!(
                kind = ?error.kind(),
                os_error = ?error.raw_os_error(),
                "the recording could not be synced to disk; the writes go on"
            );
            self.error = Some(io_error(&self.writer.files().master, &error));
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

    /// The write error, else the first failed sync's, once the thread is
    /// stopped; `None` while the loop runs, after the writer was taken, or
    /// when every write and sync succeeded.
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

    /// What the fake writer saw: the frames written, and how many had
    /// been written at each sync.
    #[derive(Default)]
    struct Seen {
        writes: usize,
        syncs: Vec<usize>,
    }

    /// Counts the writes and syncs; every sync fails when `fail_syncs` is set, with
    /// what a WebDAV mount on the Mac answers both syncs (ENOTTY).
    struct Recording {
        seen: Arc<Mutex<Seen>>,
        fail_syncs: bool,
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
            self.seen.lock().unwrap().writes += 1;
            Ok(())
        }
        fn sync(&mut self) -> std::io::Result<()> {
            let seen = &mut *self.seen.lock().unwrap();
            seen.syncs.push(seen.writes);
            if self.fail_syncs {
                Err(std::io::Error::from_raw_os_error(25))
            } else {
                Ok(())
            }
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            Ok(self.files())
        }
    }

    /// The `warn` lines logged on the test's thread.
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Log {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Three sync intervals and part of a fourth.
    const FRAMES: usize = 3 * SYNC_INTERVAL_FRAMES + 10;

    /// What a run did: what the fake saw, the errors reported, the error
    /// kept for the close, and the `warn` lines.
    struct Run {
        seen: Seen,
        reported: Vec<CaptureError>,
        kept: Option<CaptureError>,
        log: String,
    }

    /// Pushes [`FRAMES`] one-lane frames through a writer thread's worker
    /// over the fake, drained on the test's thread so its log is captured
    /// there. The relay holds them all, so none is dropped.
    fn run(fail_syncs: bool) -> Run {
        let relay = Arc::new(FrameRelay::new(1, FRAME_SIZE, FRAMES));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let reported = Arc::new(Mutex::new(Vec::new()));
        let mut thread = WriterThread::new(
            Arc::clone(&relay),
            Box::new(Recording {
                seen: Arc::clone(&seen),
                fail_syncs,
            }),
            Arc::new(LevelSlot::new(false)),
            1,
            false,
            Box::new(|_| {}),
            Box::new({
                let reported = Arc::clone(&reported);
                move |error| reported.lock().unwrap().push(error)
            }),
        );
        let zeros = vec![0.0f32; FRAME_SIZE];
        for _ in 0..FRAMES {
            assert!(relay.begin_frame());
            relay.write(0, &zeros);
            relay.end_frame();
        }
        let log = Log::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let log = log.clone();
                move || log.clone()
            })
            .with_max_level(tracing::Level::WARN)
            .finish();
        let mut worker = thread.worker.take().unwrap();
        tracing::subscriber::with_default(subscriber, || worker.drain());
        thread.worker = Some(worker);
        Run {
            seen: std::mem::take(&mut *seen.lock().unwrap()),
            reported: reported.lock().unwrap().clone(),
            kept: thread.take_error(),
            log: String::from_utf8(log.0.lock().unwrap().clone()).unwrap(),
        }
    }

    #[test]
    fn the_master_is_synced_after_every_interval_of_frames_and_not_before() {
        let run = run(false);
        assert!(run.reported.is_empty(), "{:?}", run.reported);
        assert_eq!(run.seen.syncs, [500, 1000, 1500]);
        assert!(run.kept.is_none(), "{:?}", run.kept);
        assert_eq!(run.log, "");
    }

    /// A filesystem where every sync fails, the fallback too, neither ends
    /// nor shortens the recording: every frame is written, nothing is
    /// reported while recording, and every interval tries the sync again.
    /// The failure is logged once, by kind and OS code, and kept for the
    /// close, since what was written may not be on disk.
    #[test]
    fn a_failed_sync_keeps_the_writes_going_is_logged_once_and_kept() {
        let run = run(true);
        assert!(run.reported.is_empty(), "{:?}", run.reported);
        assert_eq!(run.seen.writes, FRAMES);
        assert_eq!(run.seen.syncs, [500, 1000, 1500]);
        assert!(
            matches!(&run.kept, Some(CaptureError::WriterFailed(message)) if message.starts_with("master.caf: ")),
            "{:?}",
            run.kept
        );
        let lines: Vec<&str> = run.log.lines().collect();
        assert_eq!(lines.len(), 1, "{}", run.log);
        assert!(lines[0].contains("could not be synced"), "{}", lines[0]);
        assert!(lines[0].contains("os_error=Some(25)"), "{}", lines[0]);
        assert!(!lines[0].contains("master.caf"), "{}", lines[0]);
    }
}
