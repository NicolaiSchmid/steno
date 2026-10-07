//! The recording files: a crash-tolerant CAF master; 16 kHz WAV sidecars
//! through an exact 3:1 resampler; the writer thread, which drains the
//! relay into them and syncs every file every 5 s; `durable`, the one sync
//! every file goes through; and the readers the tests and the bench tools
//! use.
//! Swift: `Sources/StenoAudio/Writer/`.

use std::path::Path;

use crate::capture::CaptureError;

mod bytes;
pub mod caf;
mod durable;
pub mod recording_writer;
pub mod resampler;
pub mod wav;
pub mod writer_thread;

/// A failed file operation as the session reports it: the path, then the
/// OS's description.
pub(crate) fn io_error(path: &Path, error: &std::io::Error) -> CaptureError {
    CaptureError::WriterFailed(format!("{}: {error}", path.display()))
}

pub use caf::{CafFile, CafReadError, CafStreamWriter};
pub use recording_writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};
pub use resampler::Resampler48kTo16k;
pub use wav::{WavFile, WavReadError, WavStreamWriter};
pub use writer_thread::WriterThread;
