//! `steno-audio`: capture (tap and mic lanes), echo cancellation, meeting
//! detection, the recording writer and the decoder. The port of
//! `Sources/StenoAudio` (WP5 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`)
//! built on the capture spike `spikes/capture-rs`.
//!
//! # Threads and hand-offs
//!
//! One recording runs on four threads; every arrow is a hand-off through a
//! type that owns exactly that boundary. The first two arrows are
//! real-time: nothing on them allocates, locks, logs or waits (plan
//! invariant 5, proven by [`testing::rt`] in `tests/realtime.rs`).
//!
//! ```text
//! HAL IOProc thread            `realtime::io_proc::deliver`
//!   │  pointer arithmetic over `StreamLayout`, one wake
//!   ▼  ── real-time: no allocation, no locks ──
//! `LaneFrameSink` rings        `LaneRings`: all-or-nothing per callback
//!   │
//!   ▼
//! processing thread            `ProcessingThread`: 10 ms frames, echo
//!   │  cancellation, metering into `LevelSlot` atomics
//!   ▼  ── real-time: no allocation, no locks ──
//! `FrameRelay` rings           `LaneRings`: all-or-nothing per frame
//!   │
//!   ▼
//! writer thread                `WriterThread`: `RecordingWriter`,
//!   │  `Resampler48kTo16k`, files; republishes `LevelSlot` on change
//!   ▼
//! `CaptureSession`             state machine, `states`, `levels` and
//!                              `notices` channels, the asset on `stop()`
//! ```
//!
//! The synthetic backend ([`testing::SyntheticCaptureBackend`]) is a
//! producer thread speaking the `LaneFrameSink` protocol in place of the
//! IOProc; everything below it is the production path, which is what makes
//! the pipeline testable on every OS. `unsafe` lives in two places only:
//! the HAL binding module (`capture::live::hal`, macOS) and the ring
//! (`realtime::ring`); every invariant is commented there.
//!
//! Swift: `Sources/StenoAudio/StenoAudio.swift`.

// A signal-processing crate: sample counts, frame indices and dB values
// move between `usize`, `f32` and `f64` on every line, sample rates are
// compared exactly on purpose (48 000 is 48 000 or the device is wrong),
// `#[inline(always)]` marks the real-time path as Swift's
// `@inline(__always)` does, and the docs are full of HAL names (IOProc,
// CoreAudio, PipeWire) that are not code.
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
pub mod realtime;
pub mod testing;
pub mod writer;

/// The rate the aggregate device runs at and the master file is written in.
pub const SAMPLE_RATE: f64 = 48_000.0;
/// One processing frame: 10 ms at 48 kHz. The echo canceller, the level
/// meter and the writer all work in this unit.
pub const FRAME_SIZE: usize = 480;
/// The echo canceller's tail: 200 ms at 48 kHz.
pub const ECHO_TAIL_LENGTH: usize = 9_600;

pub use aec::{EchoMetrics, PassthroughEchoCanceller, SpeexEchoCanceller};
pub use capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureMode, CaptureNotice, CaptureResult,
    CaptureSession, CaptureState, CaptureStatistics, CaptureStream, DeviceChangeReason, LaneLevel,
    LaneLevels, LiveCaptureBackend, StreamLayout,
};
pub use clock::{Clock, SystemClock};
pub use codec::{CodecError, SymphoniaAudioCodec};
pub use detection::{
    MeetingDetector, MeetingEvent, ProcessAudioActivity, ProcessAudioActivitySource,
};
pub use realtime::LaneFrameSink;
pub use writer::{CafFile, RecordingWriter, WavFile};
