//! Parakeet TDT 0.6B v3 on CoreML: the `.mlmodelc` bundles FluidAudio
//! 0.17.4 ships (`parakeet-tdt-0.6b-v3`), loaded from the Swift app's model
//! directory and driven by `steno_speech`'s pipeline, the one the ONNX
//! backend runs (plan invariant 4): the VAD layout, the TDT decode loop,
//! the overlap merge, the segmentation and the language tagger. WP4b of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`, moved onto the shared
//! pipeline by A2 of `.plans/2026-10-07-stable-promotion.md`; the evidence
//! is `.plans/spikes/2026-10-01-spike-coreml-rust.md`.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift` over
//! FluidAudio's `AsrManager`.
//!
//! - [`vocab`]: `parakeet_vocab.json` read into the shared vocabulary;
//!   built everywhere, so its tests run on all three CI platforms.
//! - macOS only (plain names, because the modules do not exist in a
//!   Linux or Windows build of these docs): `coreml` (the one module
//!   allowed `unsafe`, wrapping `objc2-core-ml`), `backend` (the four
//!   model calls behind `steno_speech::SpeechBackend`), `engine` (the
//!   `SpeechEngine` implementation) and `parity` (the harness against the
//!   Swift transcripts).

#![deny(unsafe_code)]
// The docs name FluidAudio, CoreML, SentencePiece and the work packages on
// most lines; quoting each would bury the text under backticks.
#![allow(clippy::doc_markdown)]

pub mod vocab;

mod error;
pub use error::SpeechError;

#[cfg(target_os = "macos")]
pub mod backend;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod coreml;
#[cfg(target_os = "macos")]
pub mod engine;
#[cfg(target_os = "macos")]
pub mod parity;

#[cfg(target_os = "macos")]
pub use engine::{CoreMlParakeetEngine, ENGINE_ID, default_model_directory};
