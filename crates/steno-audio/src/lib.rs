//! `steno-audio`: capture (tap and mic lanes), echo cancellation, meeting
//! detection, the recording writer and the decoder. Nothing on the
//! real-time path (backend callback to sink rings, processing thread to
//! relay rings) allocates, locks, logs or waits. `unsafe` stays at the FFI
//! edges, the rings and the counting allocator. The Windows backend has
//! not run on audio hardware.

// A signal-processing crate: sample counts and dB values move between
// integer and float types everywhere, rates compare exactly on purpose,
// and the docs name HAL APIs that are not code.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::inline_always,
    clippy::doc_markdown,
    clippy::struct_excessive_bools
)]

pub mod aec;
pub mod capture;
pub mod clock;
pub mod codec;
pub mod detection;
pub mod playback;
pub mod realtime;
pub mod testing;
pub mod writer;

/// The rate the streams are opened at, every other device rate is
/// converted to, and the master file is written in.
pub const SAMPLE_RATE: f64 = 48_000.0;
/// One processing frame: 10 ms at 48 kHz, the unit of the echo canceller,
/// the level meter and the writer.
pub const FRAME_SIZE: usize = 480;
/// Processing frames a second: 48 000 samples in frames of [`FRAME_SIZE`].
pub const FRAMES_PER_SECOND: usize = SAMPLE_RATE as usize / FRAME_SIZE;
/// The echo canceller's tail: 200 ms at 48 kHz.
pub const ECHO_TAIL_LENGTH: usize = 9_600;

pub use aec::{EchoMetrics, PassthroughEchoCanceller, SpeexEchoCanceller};
pub use capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureInput, CaptureMode, CaptureNotice,
    CaptureResult, CaptureSession, CaptureState, CaptureStatistics, CaptureStream,
    DeviceChangeReason, LaneLevel, LaneLevels, LiveCaptureBackend, StreamLayout,
};
pub use clock::{Clock, SystemClock};
pub use codec::{CodecError, SymphoniaAudioCodec};
pub use detection::{
    MeetingDetector, MeetingEvent, ProcessAudioActivity, ProcessAudioActivitySource,
};
pub use playback::{Playback, PlaybackPermit, PlaybackRefused};
pub use realtime::LaneFrameSink;
pub use writer::{CafFile, CafHeader, RecordingWriter, WavFile};
