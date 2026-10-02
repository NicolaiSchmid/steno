//! The recording files: a crash-tolerant CAF master, 16 kHz WAV sidecars
//! through an exact 3:1 resampler, the writer thread that drains the relay
//! into them, and the readers the tests and the bench tools use.
//! Swift: `Sources/StenoAudio/Writer/`.

pub mod caf;
pub mod recording_writer;
pub mod resampler;
pub mod wav;
pub mod writer_thread;

pub use caf::{CafFile, CafReadError, CafStreamWriter};
pub use recording_writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};
pub use resampler::Resampler48kTo16k;
pub use wav::{WavFile, WavReadError, WavStreamWriter};
pub use writer_thread::WriterThread;
