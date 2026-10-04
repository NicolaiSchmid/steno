//! Steno's speech pipeline above the tensors: voice activity detection, the
//! pause-aligned chunker, the greedy TDT decoder, the overlap merge and the
//! mapping from pieces to segments, with the model calls behind one trait
//! (ONNX Runtime here). The `CoreML` backend runs the same decode loop
//! through [`TdtModel`]; the WP4 notes in the plan list where the rest of
//! its pipeline still differs. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md` (WP4, invariant 4) and
//! decisions 1 to 5 of `.plans/2026-10-01-cross-platform-speech-stack.md`.
//!
//! - [`backend`]: [`SpeechBackend`], the four model calls (features,
//!   encoder, decoder step, joint step), the tensors they exchange and the
//!   logits split.
//! - [`features`]: the `NeMo` mel preprocessor in Rust.
//! - [`vocab`]: `tokens.txt`, word boundaries, the splice-safe set.
//! - [`decoder`]: the greedy TDT loop over one encoder window, over
//!   [`TdtModel`].
//! - [`vad`]: [`VoiceActivityDetector`], Silero through ONNX Runtime and an
//!   energy detector for tests.
//! - [`chunker`]: the longest-pause layout with the 60 s memory clamp.
//! - [`merge`]: the time-tolerant LCS merge of overlapping windows.
//! - [`segmentation`]: pieces to words to [`RawSegment`](steno_core::RawSegment)s.
//! - [`language`]: the per-segment language tagger.
//! - [`pipeline`]: [`Transcriber`], which runs the above in order and
//!   retries empty chunks with a wider window.
//! - [`onnx`]: the ONNX Runtime backend over our fp32 export.
//! - [`model_store`]: the manifest, its two hosts and the checksummed,
//!   resumable download.
//! - [`engine`]: [`OnnxSpeechEngine`], the in-process `SpeechEngine` the
//!   sidecar hosts.
//! - [`sidecar`]: [`SidecarSpeechEngine`], the `SpeechEngine` over the
//!   `steno-speech-sidecar` child process, and its wire protocol.
//! - [`runtime`]: [`SpeechSettings`] and [`SpeechRuntime`], which engine
//!   runs on which platform.
//! - [`wav`]: the 16 kHz PCM-16 reader of the example and the tests.
//! - [`error`]: [`SpeechError`], the one error type, and [`SidecarError`],
//!   its cause when the speech sidecar fails.
//!
//! # Models
//!
//! Nothing is committed. [`ModelStore::new`] takes the store root, which
//! holds one folder per asset: `<root>/parakeet-tdt-0.6b-v3-fp32/` and
//! `<root>/silero-vad/`. Steno's root is the `onnx/` folder of its models
//! directory ([`ModelStore::in_models_directory`]). The app and the CLI
//! take the models directory from the settings, else `STENO_MODELS_DIR`,
//! else `<support directory>/Models` (`steno-services`);
//! [`ModelStore::from_environment`], for the `transcribe` example and the
//! FLEURS test, takes it from the same variable and default.
//!
//! Both assets download with their checksums verified: Silero VAD from
//! the sherpa-onnx GitHub release, the fp32 Parakeet export (2.6 GB, over
//! GitHub's 2 GB asset limit) from the Hugging Face repository
//! [`STENO_MODELS_REPO`] at the commit [`PARAKEET_V3_FP32_REVISION`]
//! ([`ModelSource::HuggingFace`], uploaded by `scripts/upload-models.sh`).
//! A mirror ([`ModelStore::with_mirror`]) serves both instead.
//!
//! # Threads and process boundaries
//!
//! Inference is synchronous and CPU-bound. [`OnnxSpeechEngine`] runs it on
//! a blocking thread when a tokio runtime is present. The app does not run
//! it in its own process (invariant 4, speech-stack decision 5): on Linux
//! and Windows it runs [`SidecarSpeechEngine`], which hosts the same
//! [`Transcriber`] in `steno-speech-sidecar` and sends it the audio over a
//! pipe, so an abort out of ONNX Runtime ends the child and not the app;
//! on macOS the in-process `CoreML` engine is the default and the sidecar
//! a fallback behind [`SpeechSettings::onnx_sidecar_on_mac`]
//! ([`runtime`]).
//!
//! ```no_run
//! use steno_core::{AudioBuffer16k, SpeechEngine};
//! use steno_speech::{ModelStore, OnnxOptions, OnnxSpeechEngine};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! let engine = OnnxSpeechEngine::new(ModelStore::from_environment(), OnnxOptions::default());
//! engine.prepare().await?;
//! let segments = engine.transcribe(&AudioBuffer16k::silence(1.0), None).await?;
//! assert!(segments.is_empty());
//! # Ok(())
//! # }
//! ```

// Signal processing counts samples, frames and bins in `usize` and turns
// them into seconds and weights in `f32`/`f64` on every other line; the
// values are tiny (a recording is under 2^32 samples) and the per-site
// allowances the other crates use would bury the arithmetic. The remaining
// pedantic lints stay on.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
// Tests compare the exact floats they constructed and assert on emptiness.
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        clippy::assert_is_empty,
        clippy::single_range_in_vec_init,
        clippy::large_stack_arrays
    )
)]

pub mod backend;
pub mod chunker;
pub mod decoder;
pub mod engine;
pub mod error;
pub mod features;
pub mod language;
pub mod merge;
pub mod model_store;
pub mod onnx;
pub mod pipeline;
pub mod runtime;
pub mod segmentation;
pub mod sidecar;
pub mod vad;
pub mod vocab;
pub mod wav;

pub use backend::{
    DecoderState, DecoderStep, EncoderOutput, FRAME_SAMPLES, FRAME_SECONDS, Features,
    JointDecision, ModelShape, SAMPLE_RATE, SpeechBackend, sample_count, split_logits,
};
pub use chunker::{Chunk, ChunkerConfig, Cut};
pub use decoder::{
    DecodeStats, Decoded, DecoderConfig, TdtModel, Token, TokenBudget, TokenDuration, WindowEnd,
    confidence, decode_frames,
};
pub use engine::OnnxSpeechEngine;
pub use error::{SidecarError, SpeechError};
pub use features::MelExtractor;
pub use language::{LanguageRecognizer, LanguageTagger, StopwordRecognizer, WhatlangRecognizer};
pub use model_store::{
    DownloadProgress, ModelAsset, ModelFile, ModelSource, ModelStore, PARAKEET_V3_FP32_REVISION,
    STENO_MODELS_REPO,
};
pub use onnx::{OnnxBackend, OnnxOptions};
pub use pipeline::{PipelineConfig, RecoveryConfig, Transcriber, Transcript};
pub use runtime::{SpeechRuntime, SpeechSettings};
pub use segmentation::{TokenAggregator, TranscriptSegmenter};
pub use sidecar::{SidecarConfig, SidecarHealth, SidecarSpeechEngine};
pub use vad::{EnergyVad, SileroVad, VadConfig, VoiceActivityDetector};
pub use vocab::Vocab;
