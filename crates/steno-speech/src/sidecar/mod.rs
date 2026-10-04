//! ONNX inference in a child process, the speech sidecar (decision 5 of
//! `.plans/2026-10-01-cross-platform-speech-stack.md`, invariant 4 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`). An uncaught C++
//! exception in ONNX Runtime ends the process, and the fp32 export works in
//! 2 to 3 GB; in `steno-speech-sidecar` such an end takes the child, not the
//! app, and the working set goes when the child does.
//!
//! - [`protocol`]: the framed JSON messages over the child's stdin and
//!   stdout, with the samples as a binary payload. No socket, no file.
//! - [`client`]: [`SidecarSpeechEngine`], the `SpeechEngine` that spawns,
//!   limits (per-request deadline, memory ceiling) and replaces the child,
//!   and [`directml_switched_off`], whether a child ended on `DirectML`.
//!
//! The binary lives in `crates/steno-speech-sidecar`; its tests kill,
//! abort, hang and overfill the child and check that the engine reports
//! the error and recovers on the next call.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

pub mod client;
pub mod protocol;

pub use client::{
    FALLBACK_NOTICE, SIDECAR_BINARY, SidecarConfig, SidecarHealth, SidecarSpeechEngine,
    directml_switched_off,
};
