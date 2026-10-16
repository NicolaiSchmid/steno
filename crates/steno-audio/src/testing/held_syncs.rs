//! Held syncs, behind the `test-support` feature: a session's writer
//! whose periodic sync of the files waits until the test lets it go, as a
//! `FlushFileBuffers` or an `fsync` on a slow disk does, so a test can
//! prove what keeps going while the writer thread is stuck in one (the
//! level stream, A20 of `.plans/2026-10-07-stable-promotion.md`). Rust
//! only: Swift synced at the close alone.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use steno_core::AudioLane;

use crate::capture::{CaptureError, RecordingWriterFactory};
use crate::writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};

/// Holds every periodic sync of the writers its
/// [`writer_factory`](Self::writer_factory) opens, until
/// [`release`](Self::release); the writes and the close's own sync are not
/// held. Release it before the session stops or drops, which waits for
/// the writer thread.
#[derive(Debug, Default)]
pub struct HeldSyncs {
    gate: Arc<Gate>,
}

#[derive(Debug, Default)]
struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct State {
    released: bool,
    /// Syncs that have begun, held or let through.
    begun: usize,
}

impl Gate {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl HeldSyncs {
    /// [`RecordingWriter::new`], its periodic syncs held; for
    /// [`CaptureSession::with_writer_factory`](crate::CaptureSession::with_writer_factory).
    #[must_use]
    pub fn writer_factory(&self) -> RecordingWriterFactory {
        let gate = Arc::clone(&self.gate);
        Arc::new(move |layout, lanes: &[AudioLane], keep_raw| {
            Ok(Box::new(HeldWriter {
                writer: RecordingWriter::new(layout, lanes, keep_raw)?,
                gate: Arc::clone(&gate),
            }) as Box<dyn RecordingWriting>)
        })
    }

    /// Waits up to `timeout` for a sync to begin, and so to be held while
    /// no release came; whether one did.
    #[must_use]
    pub fn wait_until_one_began(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.gate.lock();
        while state.begun == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .gate
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Lets the held syncs go, and every later one through.
    pub fn release(&self) {
        self.gate.lock().released = true;
        self.gate.changed.notify_all();
    }
}

/// The production writer with its periodic sync behind the gate.
struct HeldWriter {
    writer: RecordingWriter,
    gate: Arc<Gate>,
}

impl RecordingWriting for HeldWriter {
    fn files(&self) -> RecordingFiles {
        self.writer.files()
    }

    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        self.writer.write(frames)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        let mut state = self.gate.lock();
        state.begun += 1;
        self.gate.changed.notify_all();
        while !state.released {
            state = self
                .gate
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(state);
        self.writer.sync()
    }

    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        self.writer.finish()
    }

    fn created_directory(&self) -> bool {
        self.writer.created_directory()
    }
}
