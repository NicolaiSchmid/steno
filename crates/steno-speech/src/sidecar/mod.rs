//! ONNX inference in a child process (speech-stack decision 5, invariant
//! 4 of the Rust port plan). ONNX Runtime reports some failures as C++
//! exceptions that abort through the FFI, and the fp32 export works in 2 to
//! 3 GB; in `steno-speech-sidecar` an abort ends the child, not the app,
//! and the working set goes when the child does.
//!
//! - [`protocol`]: the framed JSON messages over the child's stdin and
//!   stdout, with the samples as a binary payload. No socket, no file.
//! - [`client`]: [`SidecarSpeechEngine`], the `SpeechEngine` that spawns,
//!   limits (per-request deadline, memory ceiling) and replaces the child.
//!
//! The binary lives in `crates/steno-speech-sidecar`; its tests kill,
//! abort, hang and overfill the child and check that the engine reports
//! the error and recovers on the next call.

pub mod client;
pub mod protocol;

pub use client::{SIDECAR_BINARY, SidecarConfig, SidecarHealth, SidecarSpeechEngine};
