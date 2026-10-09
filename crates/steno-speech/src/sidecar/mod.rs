//! ONNX inference in a child process, the speech sidecar (decision 5 of
//! `.plans/2026-10-01-cross-platform-speech-stack.md`, invariant 4 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`). Why speech runs in a
//! child, and on which platform: [`runtime`](crate::runtime).
//!
//! - [`protocol`]: the framed JSON messages over the child's stdin and
//!   stdout, with the samples as a binary payload. No socket, no file.
//! - [`client`]: [`SidecarSpeechEngine`], the `SpeechEngine` that spawns,
//!   limits (per-request deadline, memory ceiling) and replaces the child;
//!   [`directml_switched_off`], whether a child's end switched `DirectML`
//!   off for the rest of the app's run (which ends count: the [`client`]
//!   docs); and
//!   [`FALLBACK_NOTICE`], the start of the child's stderr line about a
//!   fallback.
//! - `scope` (Linux only): the child in a systemd scope of its own when
//!   the app runs in a unit of a systemd user manager, so that under
//!   memory pressure `systemd-oomd` can kill the child's cgroup without
//!   the app's.
//!
//! The binary lives in `crates/steno-speech-sidecar`; its tests kill,
//! abort, hang and overfill the child and check that the engine reports
//! the error and recovers on the next call.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

pub mod client;
pub mod protocol;
#[cfg(target_os = "linux")]
mod scope;

pub use client::{
    FALLBACK_NOTICE, SIDECAR_BINARY, SidecarConfig, SidecarHealth, SidecarSpeechEngine,
    directml_switched_off,
};
