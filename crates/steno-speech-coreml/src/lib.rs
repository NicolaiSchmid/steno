//! Parakeet TDT 0.6B v3 on CoreML: the `.mlmodelc` bundles FluidAudio
//! 0.17.4 ships (`parakeet-tdt-0.6b-v3`), loaded from the Swift app's model
//! directory and driven from Rust with the transcript the Swift app
//! produces. WP4b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`;
//! the evidence is `.plans/spikes/2026-10-01-spike-coreml-rust.md`.
//!
//! Two halves:
//!
//! - Platform-independent, built everywhere so its tests run on all three
//!   CI platforms: [`chunking`] (window layout, silence-aligned starts,
//!   the end-aligned final window, the adaptive speech gate), [`decoder`]
//!   (FluidAudio's TDT loop: `steno_speech`'s shared loop under
//!   FluidAudio's guards, with steps of its own), [`merge`] (the overlap merge, seam-word
//!   collapse, seam-gap splice rules),
//!   [`vocab`] (the SentencePiece vocabulary and its derived id sets),
//!   [`segments`] (tokens to timed words to `RawSegment`s, as
//!   `StenoSpeech` does it), [`wav`] (the harness's 16 kHz WAV reader) and
//!   [`wer`] (the parity scorer).
//! - macOS only (plain names, because the modules do not exist in a
//!   Linux or Windows build of these docs): `coreml` (the one module
//!   allowed `unsafe`, wrapping `objc2-core-ml`), `backend` (the four
//!   model calls: preprocessor, encoder, decoder step, joint step, and the
//!   window the shared loop decodes), `pipeline` (windows in parallel,
//!   merge, repair), `engine` (the `SpeechEngine` implementation) and
//!   `parity` (the harness against the Swift baseline).
//!
//! Every heuristic here is a port of FluidAudio's Swift; each item names
//! its origin as `Type.method` in parentheses (the file is `Type.swift`),
//! the one place a Swift pointer appears for that item. The Swift app's
//! transcript is the oracle: a Rust transcript that differs is a bug (plan
//! invariant 6). The decoder core is small and stable; the heuristics
//! around it change in most FluidAudio releases, which is why the parity
//! harness exists.
//!
//! The decode loop is `steno_speech`'s (plan invariant 4); the chunker,
//! the merge, the recovery and the segmentation are still this crate's
//! own, and the WP4 notes in the plan list where they differ.

#![deny(unsafe_code)]
// The docs name FluidAudio, CoreML, SentencePiece and the work packages on
// most lines; quoting each would bury the text under backticks.
#![allow(clippy::doc_markdown)]

pub mod chunking;
pub mod decoder;
pub mod merge;
pub mod segments;
pub mod vocab;
pub mod wav;
pub mod wer;

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
pub mod pipeline;

#[cfg(target_os = "macos")]
pub use engine::{CoreMlParakeetEngine, ENGINE_ID, default_model_directory};

/// One decoded token with its global encoder frame (80 ms), the joint's
/// probability for it and the duration bin it advanced by
/// (`ChunkProcessor.TokenWindow`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    /// SentencePiece id into the vocabulary.
    pub id: usize,
    /// Global encoder frame index at emission.
    pub frame: usize,
    /// The joint's probability for the token, clamped to `[0, 1]` as
    /// `TdtDurationMapping.clampProbability` does.
    pub confidence: f32,
    /// Frames the decoder advanced after emitting; `0` when unknown.
    pub duration: usize,
}

/// The shared loop's token, its `u32` id widened to the `usize` this
/// crate's vocabulary indexes by.
impl From<steno_speech::Token> for Token {
    fn from(token: steno_speech::Token) -> Self {
        Token {
            id: token.id as usize,
            frame: token.frame,
            confidence: token.confidence,
            duration: token.duration,
        }
    }
}
