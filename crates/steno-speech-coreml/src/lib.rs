//! Parakeet TDT 0.6B v3 on CoreML: FluidAudio's `.mlmodelc` bundles, driven
//! by the same `steno_speech` pipeline the ONNX backend runs. [`vocab`] builds
//! everywhere; `coreml` (the one module with `unsafe`), `backend`, `engine`
//! and `parity` are macOS only.

#![deny(unsafe_code)]
// The docs name FluidAudio, CoreML and SentencePiece on most lines.
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
