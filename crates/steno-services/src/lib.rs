//! The composition root of the Rust app: one function, [`build`], turns
//! the stored settings into the real object graph (store, pipeline over
//! the platform's speech engine and the ONNX diarizer, LLM passes,
//! destinations, handover listener, capture session, meeting detector,
//! secret store) and the [`steno_host::Services`] the host runs on. The
//! Tauri shell and the `steno` CLI both call it; nothing here contains
//! logic the other would not also need. Swift: `apps/macos/Steno/AppEnvironment.swift`
//! and `Sources/steno/Wiring.swift`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`, `WP6b`.
//!
//! What stays a fake or a stub here is named in [`build`]'s doc: the
//! shell's platform services (permissions, login item, updater, clip
//! player, QR encoder, the window opener) are `WP8`'s; the speech sidecar
//! process is `WP4c`'s, so ONNX inference runs in-process for now.

pub mod app;
pub mod delivery;
pub mod handover;
pub mod llm;
pub mod misc;
pub mod pipeline_service;
pub mod recorder;
pub mod secrets;
pub mod speech;

pub use app::{App, AppOptions, build};
pub use secrets::{FileSecretStore, KeyringSecretStore};
