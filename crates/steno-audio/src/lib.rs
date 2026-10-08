//! `steno-audio`: capture (tap and mic lanes), echo cancellation, meeting
//! detection, the recording writer and the decoder. The port of
//! `Sources/StenoAudio` (WP5 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`).
//!
//! - [`aec`]: echo cancellation, Speex's MDF filter over the vendored
//!   SpeexDSP and a passthrough, with the ERLE metrics.
//! - [`capture`]: the [`CaptureSession`] state machine over a
//!   [`CaptureBackend`], the configuration and results, the stream layout,
//!   and the live backend (Core Audio on macOS, PipeWire on Linux, WASAPI
//!   on Windows).
//! - [`codec`]: [`SymphoniaAudioCodec`], decoding recordings and phone
//!   files to 16 kHz mono a block at a time, and the mixdown.
//! - [`detection`]: the [`MeetingDetector`]: which processes hold the
//!   microphone, debounced into a call starting and ending, the WASAPI
//!   session mapping, and the live process-activity source on each
//!   platform.
//! - [`realtime`]: the rings, the sink, the IOProc body, the two-stream
//!   bodies, the processing thread and the relay; everything on the
//!   real-time path.
//! - [`writer`]: the recording writer (CAF master, 16 kHz WAV sidecars);
//!   its thread, which syncs every file every 5 s; `durable`, the one sync
//!   every file goes through; and the 3:1 resampler.
//! - [`clock`]: the injectable [`Clock`] the rebuild and the detector
//!   sleep on.
//! - [`testing`]: the synthetic backend, the manual clock, fixtures,
//!   a fake process-activity source and the counting allocator.
//!
//! # Threads and hand-offs
//!
//! On macOS one recording runs on four threads; every arrow is a hand-off
//! through a type that owns exactly that boundary. The first two arrows
//! are real-time: nothing on them allocates, locks, logs or waits (plan
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
//!   │  `Resampler48kTo16k`, files, a sync of every file every 5 s;
//!   │  republishes `LevelSlot` on change
//!   ▼
//! `CaptureSession`             state machine, `states`, `levels` and
//!                              `notices` channels, the asset on `stop()`
//! ```
//!
//! On Linux the top of the diagram is PipeWire's data-loop thread running
//! the capture stream's `process` callback (`capture::live::pipewire`),
//! which turns its one interleaved buffer into a view
//! ([`realtime::interleaved_view`]) and calls `deliver_slices`, the safe
//! form of the same `deliver`.
//!
//! On Windows the top of the diagram is two WASAPI capture threads
//! (`capture::live::wasapi`): the microphone thread is the first arrow,
//! routing each packet through `realtime::PacketRouter`, and the system
//! thread stages its packets into a `realtime::FollowerLane` that the
//! router pulls from.
//!
//! The synthetic backend ([`testing::SyntheticCaptureBackend`]) is a
//! producer thread speaking the `LaneFrameSink` protocol in place of the
//! IOProc; everything below it is the production path, which is what makes
//! the pipeline testable on every OS. `unsafe` is confined to the FFI
//! edges, each with its invariant beside it: the Core Audio binding
//! (`capture::live::hal`, `capture::live::backend`), the WASAPI binding
//! (`capture::live::wasapi::com`), the Speex FFI (`aec::speex`), the ring
//! and its raw-pointer callers (`realtime::ring`, `realtime::sink`,
//! `realtime::io_proc`, which also reads a mapped PipeWire buffer as
//! samples; the rest of libpipewire is reached through the safe `pipewire`
//! crate), and the counting allocator (`testing::rt`).
//!
//! # Platforms
//!
//! The live backend, the input device list and the process-activity source
//! are Core Audio on macOS, PipeWire on Linux and WASAPI on Windows. **The
//! Windows backend has not run on hardware:** no Windows machine with audio
//! devices has run it. It is written against Microsoft's documentation,
//! built, linted and tested on the `windows-latest` CI runner, which has no
//! audio endpoint (only process loopback runs there); its per-packet bodies
//! (`realtime::streams`), the stream plan (`capture::split_streams`) and
//! the session mapping (`detection::sessions`) are platform-independent and
//! tested on every OS, the zero-allocation proof included. The hardware
//! checks in `tests/live_windows.rs` are `--ignored` until a Windows
//! machine runs them.
//!
//! Swift: `Sources/StenoAudio/StenoAudio.swift`.

// A signal-processing crate: sample counts, frame indices and dB values
// move between `usize`, `f32` and `f64` on every line, sample rates are
// compared exactly on purpose (48 000 is 48 000 or the device is wrong),
// `#[inline(always)]` marks the real-time path as Swift's
// `@inline(__always)` does, and the docs are full of HAL names (IOProc,
// CoreAudio, PipeWire, WASAPI) that are not code.
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

/// The rate the aggregate device runs at, the Linux and Windows streams are
/// opened at, and the master file is written in.
pub const SAMPLE_RATE: f64 = 48_000.0;
/// One processing frame: 10 ms at 48 kHz. The echo canceller, the level
/// meter and the writer all work in this unit.
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
pub use realtime::LaneFrameSink;
pub use writer::{CafFile, RecordingWriter, WavFile};
