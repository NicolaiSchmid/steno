//! ONNX inference in a child process, the speech sidecar (decision 5 of
//! `.plans/2026-10-01-cross-platform-speech-stack.md`, invariant 4 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`). Why speech runs in a
//! child, and on which platform: [`runtime`](crate::runtime).
//!
//! - [`protocol`]: the framed JSON messages over the child's stdin and
//!   stdout, with the samples as a binary payload. No socket, no file.
//! - [`client`]: [`SidecarSpeechEngine`], the `SpeechEngine` that spawns,
//!   limits (per-request deadline, memory ceiling) and replaces the child;
//!   [`directml_switched_off`], whether a child crashed, hung or overran
//!   the memory ceiling during a load or a request on `DirectML` (an
//!   overrun between requests counts too, a death then does not); and
//!   [`FALLBACK_NOTICE`], the start of the child's stderr line about a
//!   fallback.
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
